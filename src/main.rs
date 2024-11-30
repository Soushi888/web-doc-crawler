use clap::Parser;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use url_reader::{extract_links, fetch_url_with_firefox};

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
    match fetch_url_with_firefox(&url).await {
        Ok(page) => {
            let filename = sanitize_filename(&page.title);
            let output_path = output_dir.join(format!("{}.md", filename));

            // Write content to file
            fs::write(&output_path, &page.content)?;
            println!("Wrote content to {}", output_path.display());

            // Add to index
            let mut index_content = String::new();
            index_content.push_str(&format!("# {}\n\n", page.title));
            index_content.push_str(&format!("- [{}]({}.md)\n", page.title, filename));

            // Extract and process links
            let links = extract_links(&url)?;
            for link in links {
                println!("Processing link: {}", link);
                match fetch_url_with_firefox(&link).await {
                    Ok(sub_page) => {
                        let sub_filename = sanitize_filename(&sub_page.title);
                        let sub_output_path = output_dir.join(format!("{}.md", sub_filename));

                        // Write content to file
                        fs::write(&sub_output_path, &sub_page.content)?;
                        println!("Wrote content to {}", sub_output_path.display());

                        // Add to index
                        index_content
                            .push_str(&format!("  - [{}]({}.md)\n", sub_page.title, sub_filename));
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
