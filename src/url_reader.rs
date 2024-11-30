use base64::prelude::*;
use fantoccini::ClientBuilder;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use tokio::runtime::Runtime;
use url::Url;

const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB
static OUTPUT_DIR: OnceLock<PathBuf> = OnceLock::new();

pub struct PageContent {
  pub title: String,
  pub content: String,
}

fn get_element_text(element: &ElementRef) -> String {
  element
    .text()
    .collect::<Vec<_>>()
    .join(" ")
    .trim()
    .to_string()
}

fn normalize_url(url: &str, base_url: &str) -> String {
  if url.starts_with("http://") || url.starts_with("https://") {
    url.to_string()
  } else if url.starts_with("//") {
    format!("https:{}", url)
  } else if url.starts_with('/') {
    // Get the domain from base_url
    if let Ok(base_parsed) = Url::parse(base_url) {
      format!(
        "{}://{}{}",
        base_parsed.scheme(),
        base_parsed.host_str().unwrap_or(""),
        url
      )
    } else {
      url.to_string()
    }
  } else {
    // Relative URL - combine with base URL
    if let Ok(base) = Url::parse(base_url) {
      if let Ok(absolute) = base.join(url) {
        absolute.to_string()
      } else {
        url.to_string()
      }
    } else {
      url.to_string()
    }
  }
}

struct GeckoDriver {
  process: Child,
}

impl GeckoDriver {
  fn new() -> Result<Self, Box<dyn Error>> {
    // First, try to kill any existing GeckoDriver processes
    let _ = Command::new("pkill").args(["-f", "geckodriver"]).output();

    // Wait a moment for the process to be cleaned up
    thread::sleep(Duration::from_millis(500));

    println!("Starting GeckoDriver...");
    let process = Command::new("geckodriver")
      .arg("--port")
      .arg("4444")
      .spawn()?;

    // Wait for the driver to start
    thread::sleep(Duration::from_secs(1));

    Ok(GeckoDriver { process })
  }

  fn cleanup(&mut self) {
    println!("Stopping GeckoDriver...");
    let _ = self.process.kill();
    let _ = Command::new("pkill").args(["-f", "geckodriver"]).output();
  }
}

impl Drop for GeckoDriver {
  fn drop(&mut self) {
    self.cleanup();
  }
}

pub async fn fetch_url(url: &str) -> Result<PageContent, Box<dyn Error>> {
  // Try standard request first
  if let Ok(content) = fetch_url_standard(url).await {
    if !content.content.is_empty() && content.content.len() > 200 {
      return Ok(content);
    }
  }

  // If standard request fails or returns empty content, try with Firefox
  let mut driver = GeckoDriver::new()?;
  let result = fetch_url_with_firefox(url).await;

  // Ensure cleanup happens before returning
  driver.cleanup();

  result
}

async fn fetch_url_standard(url: &str) -> Result<PageContent, Box<dyn Error>> {
  let agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

  let response = ureq::get(url)
    .set("User-Agent", agent)
    .set(
      "Accept",
      "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
    )
    .set("Accept-Language", "en-US,en;q=0.5")
    .set("Accept-Encoding", "gzip, deflate, br")
    .set("Connection", "keep-alive")
    .set("Upgrade-Insecure-Requests", "1")
    .call()?;

  let body = response.into_string()?;
  Ok(extract_content(&body, url).await)
}

async fn fetch_url_with_firefox(url: &str) -> Result<PageContent, Box<dyn Error>> {
  // Create a new tokio runtime
  let rt = Runtime::new()?;

  // Run the async code in the runtime
  let result = rt.block_on(async {
    // Configure Firefox capabilities
    let mut caps = serde_json::map::Map::new();
    let mut firefox_opts = serde_json::map::Map::new();
    firefox_opts.insert(
      "args".to_string(),
      serde_json::Value::Array(vec!["-headless".into()]),
    );
    caps.insert(
      "moz:firefoxOptions".to_string(),
      serde_json::Value::Object(firefox_opts),
    );

    // Create a new WebDriver client with retry logic
    let mut retries = 3;
    let mut last_error = None;

    while retries > 0 {
      match ClientBuilder::native()
        .capabilities(caps.clone())
        .connect("http://localhost:4444")
        .await
      {
        Ok(client) => {
          // Navigate to the URL
          match client.goto(url).await {
            Ok(_) => {
              // Wait for network idle
              tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

              // Get the page content
              match client.source().await {
                Ok(content) => {
                  let _ = client.close().await;
                  return Ok(extract_content(&content, url).await);
                }
                Err(e) => {
                  let _ = client.close().await;
                  last_error = Some(e.to_string());
                  retries -= 1;
                  thread::sleep(Duration::from_secs(1));
                  continue;
                }
              }
            }
            Err(e) => {
              let _ = client.close().await;
              last_error = Some(e.to_string());
              retries -= 1;
              thread::sleep(Duration::from_secs(1));
              continue;
            }
          }
        }
        Err(e) => {
          last_error = Some(e.to_string());
          retries -= 1;
          thread::sleep(Duration::from_secs(1));
        }
      }
    }

    Err(std::io::Error::new(
      std::io::ErrorKind::Other,
      format!(
        "Failed to connect to WebDriver after retries: {:?}",
        last_error
      ),
    ))
  });

  match result {
    Ok(page_content) => Ok(page_content),
    Err(e) => Err(Box::new(e) as Box<dyn Error>),
  }
}

async fn extract_content(html: &str, base_url: &str) -> PageContent {
  let document = Html::parse_document(html);

  // Try multiple selectors for GitBook content
  let content_selectors = [
    "div[role='main']",
    "main",
    "div.content",
    "div.markdown",
    "div.page-inner",
    "div#content",
    "div.documentation",
    "article",
    "div[class*='content']",  // Match any class containing 'content'
    "div[class*='markdown']", // Match any class containing 'markdown'
  ];

  let mut content = String::new();
  let mut title = String::new();

  // Extract title
  if let Ok(title_selector) = Selector::parse("h1") {
    if let Some(title_element) = document.select(&title_selector).next() {
      title = get_element_text(&title_element);
    }
  }

  // Try each content selector until we find content
  for selector_str in content_selectors {
    if let Ok(selector) = Selector::parse(selector_str) {
      for content_element in document.select(&selector) {
        // Skip navigation elements
        if let Some(role) = content_element.value().attr("role") {
          if role == "navigation" {
            continue;
          }
        }

        // Skip elements with navigation-related classes
        if let Some(class) = content_element.value().attr("class") {
          if class.contains("nav") || class.contains("menu") || class.contains("sidebar") {
            continue;
          }
        }

        // Pre-process HTML before conversion
        let mut html_content = content_element.html();

        // Convert <details> and <summary> to markdown
        html_content = html_content
          .replace("<details>", "\n\n<details>\n")
          .replace("</details>", "\n</details>\n\n")
          .replace("<summary>", "\n### ")
          .replace("</summary>", "\n");

        // Remove HTML classes and styles
        let re = Regex::new(r#"class="[^"]*""#).unwrap();
        html_content = re.replace_all(&html_content, "").to_string();

        let re = Regex::new(r#"style="[^"]*""#).unwrap();
        html_content = re.replace_all(&html_content, "").to_string();

        // Convert HTML to markdown first
        let mut element_content = html2md::parse_html(&html_content);

        // Find and process all image tags using regex
        let img_regex = Regex::new(r#"<img[^>]*src=["']([^"']+)["'][^>]*alt=["']([^"']*)["'][^>]*>|<img[^>]*src=["']([^"']+)["'][^>]*>"#).unwrap();

        // First pass: HTML images
        for cap in img_regex.captures_iter(&html_content) {
          let src = cap.get(1).or_else(|| cap.get(3)).map_or("", |m| m.as_str());
          let alt = cap.get(2).map_or("", |m| m.as_str());

          if let Some(downloaded_path) = download_image(src, base_url).await {
            // Replace both the HTML image tag and any markdown version that might exist
            let img_md = format!("![{}]({})", alt, downloaded_path);
            element_content = element_content.replace(&format!("![{}]({})", alt, src), &img_md);
            element_content = element_content.replace(src, &downloaded_path);
          }
        }

        // Second pass: Find markdown-style images and collect replacements
        let md_img_regex = Regex::new(r"!\[[^\]]*\]\(([^)]+)\)").unwrap();
        let mut replacements = Vec::new();

        for cap in md_img_regex.captures_iter(&element_content) {
          if let Some(src_match) = cap.get(1) {
            let src = src_match.as_str();
            if let Some(downloaded_path) = download_image(src, base_url).await {
              replacements.push((format!("({})", src), format!("({})", downloaded_path)));
            }
          }
        }

        // Apply collected replacements
        for (from, to) in replacements {
          element_content = element_content.replace(&from, &to);
        }

        if !element_content.trim().is_empty() {
          content.push_str(&element_content);
          content.push_str("\n\n");
        }
      }
      if !content.is_empty() {
        break;
      }
    }
  }

  // If no content found through selectors, try getting the body
  if content.is_empty() {
    if let Ok(body_selector) = Selector::parse("body") {
      if let Some(body_element) = document.select(&body_selector).next() {
        content = html2md::parse_html(&body_element.html());
      }
    }
  }

  // Process links
  if let Ok(link_selector) = Selector::parse("a") {
    for link in document.select(&link_selector) {
      if let Some(href) = link.value().attr("href") {
        let relative_path = convert_to_relative_path(href, base_url);
        content = content.replace(href, &relative_path);
      }
    }
  }

  // Clean up the content
  content = content
    .lines()
    .filter(|line| {
      let line = line.trim();
      !line.is_empty()
                && !line.contains("Last updated")
                && !line.contains("Previous")
                && !line.contains("Next")
                && !line.contains("Table of contents")
                && !line.contains("In this article")
                && !line.contains("On this page")
                && !line.contains("Contents")
                && !line.contains(">")  // Remove empty blockquotes
                && !line.contains("==========")  // Remove header underlines
                && !line.contains("----------") // Remove subheader underlines
    })
    .collect::<Vec<_>>()
    .join("\n\n");

  // Additional cleanup
  content = content
    .replace(r#"<div>"#, "")
    .replace(r#"</div>"#, "")
    .replace(r#"<span>"#, "")
    .replace(r#"</span>"#, "")
    .replace(r#"<p>"#, "")
    .replace(r#"</p>"#, "\n\n")
    .replace("&nbsp;", " ")
    .replace("&quot;", "\"")
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&amp;", "&")
    .replace("\n\n###\n\n", "\n\n### ") // Fix heading formatting
    .replace("\n\n##\n\n", "\n\n## ")
    .replace("\n\n#\n\n", "\n\n# ")
    .replace(" ###", "###") // Remove extra spaces before headings
    .replace(" ##", "##")
    .replace(" #", "#");

  // Remove multiple newlines
  let re = Regex::new(r"\n{3,}").unwrap();
  content = re.replace_all(&content, "\n\n").to_string();

  // Remove empty headings
  let re = Regex::new(r"###\s*\n").unwrap();
  content = re.replace_all(&content, "").to_string();

  // Remove repeated link references
  let re = Regex::new(r"\[.*?\]\(.*?\)\s*\[.*?\]\(.*?\)").unwrap();
  content = re.replace_all(&content, "").to_string();

  // Fix code block formatting
  let re = Regex::new(r"```\s*\n\s*```").unwrap();
  content = re.replace_all(&content, "").to_string();

  // Fix multiple spaces
  let re = Regex::new(r" {2,}").unwrap();
  content = re.replace_all(&content, " ").to_string();

  // Fix heading spacing
  let re = Regex::new(r"\n\n(#{1,6})\s+").unwrap();
  content = re.replace_all(&content, "\n\n$1 ").to_string();

  PageContent {
    title: if title.is_empty() {
      "Untitled".to_string()
    } else {
      title
    },
    content,
  }
}

pub fn extract_links(url: &str) -> Result<Vec<String>, Box<dyn Error>> {
  let response = ureq::get(url)
    .set(
      "User-Agent",
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
    )
    .call()?;
  let body = response.into_string()?;
  let document = Html::parse_document(&body);

  let selectors = [
    "a[href]",
    "nav a[href]",
    ".md-nav a[href]",
    ".md-content a[href]",
    ".navigation a[href]",
    ".menu a[href]",
    ".sidebar a[href]",
  ];

  let mut links = HashSet::new();

  // Parse the base URL for comparison
  let base_url = Url::parse(url).ok();
  let base_host = base_url.as_ref().and_then(|u| u.host_str()).unwrap_or("");

  for selector_str in selectors {
    if let Ok(selector) = Selector::parse(selector_str) {
      for element in document.select(&selector) {
        if let Some(href) = element.value().attr("href") {
          // Clean the href by removing fragments and query parameters
          let clean_href = href
            .split('#')
            .next()
            .unwrap_or(href)
            .split('?')
            .next()
            .unwrap_or(href)
            .trim();

          // Skip empty links, anchors, and javascript
          if clean_href.is_empty()
            || clean_href == "/"
            || clean_href.starts_with("javascript:")
            || clean_href.starts_with("mailto:")
            || clean_href.starts_with("tel:")
            || clean_href.ends_with(".pdf")
          // Skip PDFs
          {
            continue;
          }

          let normalized = normalize_url(clean_href, url);

          // Try to parse the normalized URL
          if let Ok(parsed_url) = Url::parse(&normalized) {
            // Only include links from the same domain
            if let Some(host) = parsed_url.host_str() {
              if host == base_host {
                // Remove trailing slashes for consistency
                let final_url = normalized.trim_end_matches('/').to_string();
                links.insert(final_url);
              }
            }
          }
        }
      }
    }
  }

  Ok(links.into_iter().collect())
}

pub fn set_output_dir(dir: PathBuf) {
  let _ = OUTPUT_DIR.set(dir);
}

fn get_output_dir() -> &'static PathBuf {
  OUTPUT_DIR.get().expect("Output directory not set")
}

fn extract_original_url(url: &str) -> String {
  if url.contains("gitbook.io") || url.contains("gitbook/image") {
    // Extract Imgur ID from the URL if present
    if let Some(imgur_id) = url.find("imgur.com") {
      if let Some(start) = url[..imgur_id].rfind('/') {
        if let Some(end) = url[imgur_id..].find('&') {
          let imgur_path = &url[start + 1..imgur_id + end];
          // Convert to direct Imgur URL
          return format!(
            "https://i.imgur.com/{}.png",
            imgur_path.split('/').last().unwrap_or("")
          );
        }
      }
    }
  }
  url.to_string()
}

async fn download_image(url: &str, base_url: &str) -> Option<String> {
  // Handle base64 encoded images
  if url.starts_with("data:image/") {
    return handle_base64_image(url);
  }

  let client = ClientBuilder::native()
    .connect("http://localhost:4444")
    .await
    .ok()?;

  // Resolve relative URL if needed
  let absolute_url = if url.starts_with("http") {
    url.to_string()
  } else {
    format!("{}{}", base_url.trim_end_matches('/'), url)
  };

  // Extract original URL if it's a proxy URL
  let original_url = extract_original_url(&absolute_url);
  println!("Downloading image from: {}", original_url);

  // Create a unique filename
  let mut hasher = Sha256::new();
  hasher.update(original_url.as_bytes());
  let hash = format!("{:x}", hasher.finalize());
  let filename = format!("{}.{}", &hash[..16], guess_extension(&original_url));

  // Create images directory if it doesn't exist
  let images_dir = PathBuf::from("images");
  if !images_dir.exists() {
    fs::create_dir_all(&images_dir).ok()?;
  }

  let image_path = images_dir.join(&filename);

  // Check if image already exists
  if image_path.exists() {
    return Some(format!("images/{}", filename));
  }

  // Download image
  match client.goto(&original_url).await {
    Ok(_) => {
      match client.source().await {
        Ok(content) => {
          let _ = client.close().await;

          // Write content to file
          if fs::write(&image_path, content.as_bytes()).is_ok() {
            Some(format!("images/{}", filename))
          } else {
            None
          }
        }
        Err(e) => {
          eprintln!("Failed to get page source for image {}: {}", url, e);
          let _ = client.close().await;
          None
        }
      }
    }
    Err(e) => {
      eprintln!("Failed to navigate to image {}: {}", url, e);
      let _ = client.close().await;
      None
    }
  }
}

fn handle_base64_image(data_url: &str) -> Option<String> {
  let parts: Vec<&str> = data_url.split(',').collect();
  if parts.len() != 2 {
    return None;
  }

  let metadata = parts[0];
  let base64_data = parts[1];

  // Extract image format
  let format = metadata
    .split(';')
    .next()?
    .split('/')
    .nth(1)?
    .to_lowercase();

  // Decode base64
  let bytes = BASE64_STANDARD.decode(base64_data).ok()?;

  // Check file size
  if bytes.len() > MAX_IMAGE_SIZE {
    eprintln!("Base64 image too large: {} bytes", bytes.len());
    return None;
  }

  // Create hash of content for filename
  let mut hasher = Sha256::new();
  hasher.update(&bytes);
  let hash = format!("{:x}", hasher.finalize());
  let filename = format!("{}.{}", &hash[..8], format);

  // Create images directory if it doesn't exist
  let images_dir = get_output_dir().join("images");
  fs::create_dir_all(&images_dir).ok()?;

  // Save image
  let image_path = images_dir.join(&filename);
  if image_path.exists() {
    return Some(format!("images/{}", filename));
  }

  match fs::write(&image_path, bytes) {
    Ok(_) => Some(format!("images/{}", filename)),
    Err(e) => {
      eprintln!("Failed to save base64 image: {}", e);
      None
    }
  }
}

fn guess_extension(url: &str) -> String {
  if let Some(ext) = url.split('.').last() {
    match ext.to_lowercase().as_str() {
      "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" => ext.to_string(),
      _ => "png".to_string(), // Default to png if extension is not recognized
    }
  } else {
    "png".to_string() // Default extension
  }
}

fn convert_to_relative_path(href: &str, base_url: &str) -> String {
  if href.starts_with("http") {
    // For external links, keep as is
    href.to_string()
  } else {
    // For internal links, convert to relative path
    let base = Url::parse(base_url).unwrap();
    let absolute = base.join(href).unwrap();

    if absolute.host_str() == base.host_str() {
      // Internal link - convert to relative markdown path
      let path = absolute.path();
      let mut relative_path = path.trim_start_matches('/').to_string();

      // Convert .html to .md
      if relative_path.ends_with(".html") {
        relative_path = relative_path.replace(".html", ".md");
      } else if !relative_path.contains('.') && !relative_path.is_empty() {
        relative_path = format!("{}.md", relative_path);
      }

      relative_path
    } else {
      // External link - keep as is
      href.to_string()
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn test_fetch_url() {
    let test_url = "https://www.valueflo.ws";
    let result = fetch_url(test_url).await;
    assert!(result.is_ok());
  }
}
