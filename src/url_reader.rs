use base64::prelude::*;
use fantoccini::ClientBuilder;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use sha2::{Digest, Sha256};
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

#[derive(Debug)]
pub enum Crawler {
    Network(String),
    Parsing(String),
    Browser(String),
    Io(std::io::Error),
}

impl std::error::Error for Crawler {}

impl std::fmt::Display for Crawler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Crawler::Network(msg) => write!(f, "Network error: {}", msg),
            Crawler::Parsing(msg) => write!(f, "Parsing error: {}", msg),
            Crawler::Browser(msg) => write!(f, "Browser error: {}", msg),
            Crawler::Io(err) => write!(f, "IO error: {}", err),
        }
    }
}

impl From<std::io::Error> for Crawler {
    fn from(err: std::io::Error) -> Self {
        Crawler::Io(err)
    }
}

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
    fn new() -> Result<Self, Crawler> {
        // First, try to kill any existing GeckoDriver processes
        let _ = Command::new("pkill").args(["-f", "geckodriver"]).output();

        // Wait a moment for the process to be cleaned up
        thread::sleep(Duration::from_millis(500));

        println!("Starting GeckoDriver...");
        let process = Command::new("geckodriver")
            .arg("--port")
            .arg("4444")
            .spawn()
            .map_err(|e| Crawler::Browser(e.to_string()))?;

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

pub async fn fetch_url_with_firefox(url: &str) -> Result<PageContent, Crawler> {
    let rt = Runtime::new().map_err(Crawler::Io)?;

    rt.block_on(async {
        let mut driver = None;
        let mut last_error = None;
        let mut retries = 0;
        let max_retries = 3;

        while retries < max_retries {
            match GeckoDriver::new() {
                Ok(d) => {
                    driver = Some(d);
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    retries += 1;
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }

        if let Some(mut driver) = driver {
            // Create capabilities using serde_json's Map
            let mut caps = serde_json::Map::new();
            let mut firefox_opts = serde_json::Map::new();
            firefox_opts.insert(
                "args".to_string(),
                serde_json::Value::Array(vec![serde_json::Value::String("--headless".to_string())]),
            );
            caps.insert(
                "moz:firefoxOptions".to_string(),
                serde_json::Value::Object(firefox_opts),
            );

            let client = match ClientBuilder::native()
                .capabilities(caps)
                .connect("http://localhost:4444")
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    return Err(Crawler::Browser(format!(
                        "Failed to connect to WebDriver: {}",
                        e
                    )));
                }
            };

            match client.goto(url).await {
                Ok(_) => {
                    thread::sleep(Duration::from_secs(2));

                    match client.source().await {
                        Ok(html) => {
                            let content = extract_content(&html, url).await?;
                            driver.cleanup();
                            Ok(content)
                        }
                        Err(e) => Err(Crawler::Browser(format!(
                            "Failed to get page source: {}",
                            e
                        ))),
                    }
                }
                Err(e) => Err(Crawler::Browser(format!(
                    "Failed to navigate to URL: {}",
                    e
                ))),
            }
        } else {
            Err(Crawler::Browser(format!(
                "Failed to connect to WebDriver after retries: {:?}",
                last_error
            )))
        }
    })
}

async fn extract_content(html: &str, base_url: &str) -> Result<PageContent, Crawler> {
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
                    if class.contains("nav") || class.contains("menu") || class.contains("sidebar")
                    {
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
                        element_content =
                            element_content.replace(&format!("![{}]({})", alt, src), &img_md);
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
                            replacements
                                .push((format!("({})", src), format!("({})", downloaded_path)));
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

    Ok(PageContent {
        title: if title.is_empty() {
            "Untitled".to_string()
        } else {
            title
        },
        content,
    })
}

pub fn extract_links(url: &str) -> Result<Vec<String>, Crawler> {
    let response = ureq::get(url)
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36",
        )
        .call()
        .map_err(|e| Crawler::Network(e.to_string()))?;

    let body = response
        .into_string()
        .map_err(|e| Crawler::Parsing(e.to_string()))?;

    let document = Html::parse_document(&body);

    let selectors = [
        Selector::parse("a[href]").unwrap(),
        Selector::parse("link[href]").unwrap(),
    ];

    let mut links = Vec::new();
    let base_url_parsed =
        Url::parse(url).map_err(|_| Crawler::Parsing("Invalid base URL".to_string()))?;

    for selector in &selectors {
        for element in document.select(selector) {
            if let Some(href) = element.value().attr("href") {
                let normalized_url = normalize_url(href, url);

                // Filter out external links and fragment identifiers
                if normalized_url.starts_with(base_url_parsed.as_str())
                    && !normalized_url.contains('#')
                    && !links.contains(&normalized_url)
                {
                    links.push(normalized_url);
                }
            }
        }
    }

    Ok(links)
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
    let images_dir = get_output_dir().join("images");
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
            "pdf" => "pdf".to_string(),
            _ => "".to_string(), // Default to empty string if extension is not recognized
        }
    } else {
        "".to_string() // Default extension
    }
}

fn convert_to_relative_path(href: &str, base_url: &str) -> String {
    // Remove the base URL from the href
    let url_without_base = href.replace(base_url, "");

    // Remove leading slashes
    let path = url_without_base.trim_start_matches('/');

    // Convert HTML to markdown
    if path.ends_with(".html") {
        path.replace(".html", ".md")
    } else {
        path.to_string()
    }
}

pub fn set_output_dir(dir: PathBuf) {
    let _ = OUTPUT_DIR.set(dir);
}

pub fn get_output_dir() -> &'static PathBuf {
    OUTPUT_DIR.get().expect("Output directory not set")
}

pub fn extract_original_url(url: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url() {
        assert_eq!(
            normalize_url("/docs/guide", "https://example.com"),
            "https://example.com/docs/guide"
        );
        assert_eq!(
            normalize_url("//cdn.example.com/image.jpg", "https://example.com"),
            "https://cdn.example.com/image.jpg"
        );
        assert_eq!(
            normalize_url("https://other.com/path", "https://example.com"),
            "https://other.com/path"
        );
    }

    #[test]
    fn test_guess_extension() {
        assert_eq!(guess_extension("image.jpg"), "jpg");
        assert_eq!(guess_extension("file.png"), "png");
        assert_eq!(guess_extension("doc.pdf"), "pdf");
        assert_eq!(guess_extension("noextension"), "");
    }

    #[test]
    fn test_convert_to_relative_path() {
        assert_eq!(
            convert_to_relative_path("https://example.com/docs/guide.html", "https://example.com"),
            "docs/guide.md"
        );
        assert_eq!(
            convert_to_relative_path("/docs/guide.html", "https://example.com"),
            "docs/guide.md"
        );
    }
}
