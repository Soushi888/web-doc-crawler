use clap::Parser;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use url_reader::{extract_links, fetch_url};

mod url_reader;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
  /// Base URL to crawl
  #[arg(short, long)]
  url: String,

  /// Output directory name (default: derived from URL)
  #[arg(short, long)]
  output: Option<String>,

  /// Maximum number of pages to crawl
  #[arg(short, long, default_value_t = 100)]
  max_pages: usize,

  /// Maximum depth to crawl
  #[arg(short, long, default_value_t = 3)]
  depth: usize,
}

#[derive(Debug)]
struct PageInfo {
  url: String,
  title: String,
  file_path: String,
}

fn create_file_path(url: &str, output_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
  let url = url.trim_end_matches('/');
  let parts: Vec<&str> = url.split("://").nth(1).unwrap_or(url).split('/').collect();

  // Skip the domain (valueflo.ws)
  let path_parts: Vec<&str> = parts
    .iter()
    .skip(1) // Skip domain
    .filter(|part| !part.is_empty())
    .copied()
    .collect();

  // Create the directory path
  let mut current_path = output_dir.to_path_buf();

  // If we have path parts, create the directory structure
  if !path_parts.is_empty() {
    for part in &path_parts[..path_parts.len() - 1] {
      current_path = current_path.join(part);
      if !current_path.exists() {
        fs::create_dir_all(&current_path)?;
      }
    }

    // Create filename from the last part
    let filename = if let Some(last_part) = path_parts.last() {
      if last_part.contains('.') {
        // Convert HTML files to markdown
        if last_part.ends_with(".html") {
          last_part.replace(".html", ".md")
        } else {
          last_part.to_string()
        }
      } else {
        format!("{}.md", last_part)
      }
    } else {
      "index.md".to_string()
    };

    current_path = current_path.join(filename);
  } else {
    // Root URL - create index.md in the root directory
    current_path = current_path.join("index.md");
  }

  Ok(current_path)
}

fn save_content_to_file(url: &str, content: &str, file_path: &Path) -> Result<(), Box<dyn Error>> {
  // Create parent directories if they don't exist
  if let Some(parent) = file_path.parent() {
    fs::create_dir_all(parent)?;
  }

  // Create a header with the source URL and metadata
  let header = format!("Source: [{}]({})\n\n---\n\n", url, url);
  let full_content = header + content;

  fs::write(file_path, full_content)?;
  println!("Saved content to {}", file_path.display());
  Ok(())
}

fn generate_index(pages: &HashMap<String, PageInfo>) -> String {
  let mut index = String::from("# ValueFlows Documentation Index\n\n");
  index.push_str("This index was automatically generated from the ValueFlows website.\n\n");

  // Create section headers
  index.push_str("## Contents\n\n");
  index.push_str("- [Introduction](#introduction)\n");
  index.push_str("- [Specification](#specification)\n");
  index.push_str("- [Concepts](#concepts)\n");
  index.push_str("- [Examples](#examples)\n");
  index.push_str("- [Appendix](#appendix)\n\n");

  // Helper function to add page to a section
  fn add_page_to_section(page: &PageInfo, section_content: &mut String, depth: usize) {
    let indent = "  ".repeat(depth);
    section_content.push_str(&format!(
      "{}* [{}]({}) ([source]({}))\n",
      indent, page.title, page.file_path, page.url
    ));
  }

  // Organize pages by section
  let mut introduction = String::new();
  let mut specification = String::new();
  let mut concepts = String::new();
  let mut examples = String::new();
  let mut appendix = String::new();
  let mut other = String::new();

  for page in pages.values() {
    let path = page.file_path.to_lowercase();
    if path.contains("introduction") {
      add_page_to_section(page, &mut introduction, 0);
    } else if path.contains("specification") || path.contains("spec") {
      add_page_to_section(page, &mut specification, 0);
    } else if path.contains("concepts") {
      add_page_to_section(page, &mut concepts, 0);
    } else if path.contains("examples") || path.contains("ex-") {
      add_page_to_section(page, &mut examples, 0);
    } else if path.contains("appendix") {
      add_page_to_section(page, &mut appendix, 0);
    } else {
      add_page_to_section(page, &mut other, 0);
    }
  }

  // Add sections to index
  if !introduction.is_empty() {
    index.push_str("## Introduction\n\n");
    index.push_str(&introduction);
    index.push('\n');
  }

  if !specification.is_empty() {
    index.push_str("## Specification\n\n");
    index.push_str(&specification);
    index.push('\n');
  }

  if !concepts.is_empty() {
    index.push_str("## Concepts\n\n");
    index.push_str(&concepts);
    index.push('\n');
  }

  if !examples.is_empty() {
    index.push_str("## Examples\n\n");
    index.push_str(&examples);
    index.push('\n');
  }

  if !appendix.is_empty() {
    index.push_str("## Appendix\n\n");
    index.push_str(&appendix);
    index.push('\n');
  }

  if !other.is_empty() {
    index.push_str("## Other Pages\n\n");
    index.push_str(&other);
    index.push('\n');
  }

  index
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
  let args = Args::parse();
  let url = args.url;
  let output_dir = PathBuf::from(args.output.unwrap_or_else(|| "docs".to_string()));

  // Set output directory
  url_reader::set_output_dir(output_dir.clone());

  // Create output directory if it doesn't exist
  if !output_dir.exists() {
    fs::create_dir_all(&output_dir)?;
  }

  // Create images directory if it doesn't exist
  let images_dir = output_dir.join("images");
  if !images_dir.exists() {
    fs::create_dir_all(&images_dir)?;
  }

  // Process initial URL
  println!("Fetching content from {}", url);
  match fetch_url(&url).await {
    Ok(page) => {
      let filename = sanitize_filename(&page.title);
      let output_path = output_dir.join(format!("{}.md", filename));

      // Write content to file
      fs::write(&output_path, &page.content)?;
      println!("Wrote content to {}", output_path.display());

      // Add to index
      let mut index_content = String::new();
      index_content.push_str(&format!("# {}\n\n", page.title));
      index_content.push_str(&format!(
        "- [{}]({})\n",
        page.title,
        format!("{}.md", filename)
      ));

      // Extract and process links
      let links = extract_links(&url)?;
      for link in links {
        println!("Processing link: {}", link);
        match fetch_url(&link).await {
          Ok(sub_page) => {
            let sub_filename = sanitize_filename(&sub_page.title);
            let sub_output_path = output_dir.join(format!("{}.md", sub_filename));

            // Write content to file
            fs::write(&sub_output_path, &sub_page.content)?;
            println!("Wrote content to {}", sub_output_path.display());

            // Add to index
            index_content.push_str(&format!(
              "  - [{}]({})\n",
              sub_page.title,
              format!("{}.md", sub_filename)
            ));
          }
          Err(e) => println!("Error fetching content: {}: {}", link, e),
        }
      }

      // Write index file
      let index_path = output_dir.join("index.md");
      fs::write(&index_path, index_content)?;
      println!("Wrote index to {}", index_path.display());
    }
    Err(e) => println!("Error fetching content: {}: {}", url, e),
  }

  Ok(())
}

fn sanitize_filename(filename: &str) -> String {
  filename.replace(|c: char| !c.is_alphanumeric(), "-")
}
