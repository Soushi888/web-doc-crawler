# Web Documentation Crawler

## Overview

This is a powerful Rust-based web documentation crawler designed to automatically extract and convert documentation from websites into structured markdown format. The crawler uses asynchronous programming techniques and supports headless browser interactions to capture complex web content.

## Features

- 🌐 Crawl web documentation sites
- 📄 Convert web content to markdown
- 🖼️ Download and embed images
- 🚀 Asynchronous, non-blocking design
- 🔍 Supports dynamic content rendering

## Prerequisites

- Rust (stable channel)
- Cargo
- Firefox WebDriver (GeckoDriver)
- Tokio async runtime

## Installation

1. Clone the repository
2. Ensure Firefox and GeckoDriver are installed
3. Run `cargo build`

## Usage

```bash
cargo run -- --url https://example.com/docs
```

## Configuration

The crawler supports various configuration options:
- URL crawling
- Image download limits
- Output directory customization

## Technical Details

### Async Architecture
- Uses Tokio for async runtime
- Non-blocking I/O operations
- Headless browser interactions

### Content Extraction
- Supports multiple content types
- Handles relative and absolute URLs
- Image download with extension guessing

### Error Handling
- Comprehensive error logging
- Graceful failure modes
- Configurable retry mechanisms

## Dependencies

- `fantoccini`: WebDriver interactions
- `tokio`: Async runtime
- `scraper`: HTML parsing
- `base64`: Image processing
- `sha2`: Content hashing

## Limitations

- 10MB image download limit
- Limited support for highly dynamic content
- Requires Firefox WebDriver

## Contributing

1. Fork the repository
2. Create a feature branch
3. Commit your changes
4. Push and create a Pull Request

## License

[Specify your license here]

## Contact

[Your contact information]
