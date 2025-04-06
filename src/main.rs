use actix_files::NamedFile;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, get, web};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone)]
struct MarkdownFile {
    title: String,
    path: PathBuf,
    html_path: PathBuf,
}

// App state to keep track of markdown files
struct AppState {
    output_dir: PathBuf,
}

// Add this struct to track alert state
#[derive(Default)]
struct AlertState {
    in_alert: bool,
    alert_type: Option<String>,
    buffer: String,
    alert_started: bool,
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <markdown_directory>", args[0]);
        std::process::exit(1);
    }

    // Create necessary directories
    let markdown_dir = PathBuf::from(&args[1]);
    let output_dir = markdown_dir.join("html_output");
    let static_dir = output_dir.join("static");

    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&static_dir)?;

    // Create CSS file for styling
    create_css_file(&static_dir)?;

    // Scan for markdown files and convert them
    let md_files = scan_and_convert_markdown_files(&markdown_dir, &output_dir)?;

    // Create index.html
    create_index_html(&md_files, &output_dir)?;

    println!("Server starting at http://127.0.0.1:8080");
    println!("Serving markdown files from: {}", markdown_dir.display());

    // Start web server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(AppState {
                output_dir: output_dir.clone(),
            }))
            .service(index)
            .service(actix_files::Files::new("/static", static_dir.clone()))
            .service(actix_files::Files::new("/", output_dir.clone()).index_file("index.html"))
    })
    .bind("127.0.0.1:8080")?;

    println!("Server is ready to accept connections");
    actix_web::rt::System::new().block_on(server.run())?;

    Ok(())
}

#[get("/")]
async fn index(data: web::Data<AppState>, req: HttpRequest) -> impl Responder {
    let path = data.output_dir.join("index.html");
    match NamedFile::open(path) {
        Ok(file) => file.into_response(&req),
        Err(_) => HttpResponse::NotFound().body("Index not found"),
    }
}

fn scan_and_convert_markdown_files(
    markdown_dir: &Path,
    output_dir: &Path,
) -> io::Result<Vec<MarkdownFile>> {
    let mut md_files = Vec::new();

    for entry in WalkDir::new(markdown_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
    {
        let path = entry.path();
        let rel_path = path.strip_prefix(markdown_dir).unwrap_or(path);
        let html_rel_path = rel_path.with_extension("html");
        let html_path = output_dir.join(&html_rel_path);

        // Create parent directories if they don't exist
        if let Some(parent) = html_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Read markdown content
        let mut md_content = String::new();
        File::open(path)?.read_to_string(&mut md_content)?;

        // Get title from first line or filename
        let title = md_content
            .lines()
            .next()
            .and_then(|line| {
                if line.starts_with("# ") {
                    Some(line.strip_prefix("# ").unwrap().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_string()
            });

        // Convert markdown to HTML with custom link handling
        let mut options = Options::empty();
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_FOOTNOTES);
        options.insert(Options::ENABLE_TASKLISTS);

        let parser = Parser::new_ext(&md_content, options);

        // Transform .md links to .html and handle GitHub-style alerts
        let mut alert_state = AlertState::default();
        let parser = parser.map(|event| {
            match event {
                Event::Start(Tag::BlockQuote) => {
                    alert_state.in_alert = true;
                    alert_state.buffer.clear();
                    // Don't emit the blockquote tag yet, wait to see if it's an alert
                    Event::Text(CowStr::from(""))
                }
                Event::End(Tag::BlockQuote) => {
                    let result = if alert_state.alert_type.take().is_some() {
                        Event::Html(CowStr::from("</blockquote>"))
                    } else {
                        // If it wasn't an alert, we need to emit both tags
                        Event::Html(CowStr::from("<blockquote></blockquote>"))
                    };
                    alert_state.in_alert = false;
                    alert_state.alert_started = false;
                    alert_state.buffer.clear();
                    result
                }
                Event::Text(text) if alert_state.in_alert => {
                    let text_str = text.to_string();

                    if !alert_state.alert_started {
                        alert_state.buffer.push_str(&text_str);

                        // Check if we have accumulated a complete alert marker
                        if let Some(alert_text) = alert_state.buffer.strip_prefix("[!") {
                            if let Some(end_idx) = alert_text.find(']') {
                                let alert_type = alert_text[..end_idx].to_lowercase();
                                alert_state.alert_type = Some(alert_type.clone());
                                alert_state.alert_started = true;

                                // Return the opening tag and any remaining text
                                let remaining_text = &alert_text[end_idx + 1..];
                                if remaining_text.is_empty() {
                                    Event::Html(CowStr::from(format!(
                                        "<blockquote class=\"alert alert-{}\">",
                                        alert_type
                                    )))
                                } else {
                                    Event::Html(CowStr::from(format!(
                                        "<blockquote class=\"alert alert-{}\">{}",
                                        alert_type, remaining_text
                                    )))
                                }
                            } else {
                                // Still accumulating the alert marker
                                Event::Text(CowStr::from(""))
                            }
                        } else if alert_state.buffer.len() > 10 {
                            // If we've accumulated too much text without finding an alert marker,
                            // treat it as a regular blockquote and emit the opening tag
                            let text = std::mem::take(&mut alert_state.buffer);
                            Event::Html(CowStr::from(format!("<blockquote>{}", text)))
                        } else {
                            // Still accumulating potential alert marker
                            Event::Text(CowStr::from(""))
                        }
                    } else {
                        // We're already in an alert, pass through the text
                        Event::Text(text)
                    }
                }
                Event::Start(Tag::Link(_, url, _)) => {
                    let new_url = if url.ends_with(".md") {
                        let url_str = url.into_string();
                        CowStr::from(url_str.replace(".md", ".html"))
                    } else {
                        url
                    };
                    Event::Html(CowStr::from(format!(
                        r#"<a href="{}" class="nav-button">"#,
                        new_url
                    )))
                }
                Event::End(Tag::Link(_, _, _)) => Event::Html(CowStr::from("</a>")),
                e => e,
            }
        });

        let mut html_output = String::new();
        html::push_html(&mut html_output, parser);

        // Add navigation button styling to links containing "Previous" or "Next"
        let html_output = html_output.replace(r#"<a href="/"#, r#"<a class="nav-button" href="/"#);

        // Generate HTML file with template
        let highlighted_html = syntax_highlight_code_blocks(&html_output);
        let html_content = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{}</title>
    <link rel="stylesheet" href="/static/style.css">
    <link rel="stylesheet" href="/static/prism.css">
</head>
<body>
    <header>
        <h1>{}</h1>
        <a href="/" class="home-link">← Back to Index</a>
    </header>
    <main>
        <article class="markdown-content">
            {}
        </article>
    </main>
    <footer>
        <p>Generated with love</p>
    </footer>
    <script src="/static/prism.js"></script>
</body>
</html>"#,
            title, title, highlighted_html
        );

        // Write HTML file
        let mut file = File::create(&html_path)?;
        file.write_all(html_content.as_bytes())?;

        md_files.push(MarkdownFile {
            title,
            path: path.to_path_buf(),
            html_path,
        });
    }

    // Sort by numeric prefix first, then by title
    md_files.sort_by(|a, b| {
        // Extract numeric prefix if it exists
        let get_prefix = |s: &str| {
            s.split_once('-')
                .and_then(|(prefix, _)| prefix.parse::<u32>().ok())
        };

        let a_prefix = get_prefix(&a.path.file_stem().unwrap().to_string_lossy());
        let b_prefix = get_prefix(&b.path.file_stem().unwrap().to_string_lossy());

        // Sort by numeric prefix first if both have prefixes
        match (a_prefix, b_prefix) {
            (Some(a_num), Some(b_num)) => a_num.cmp(&b_num),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        }
    });

    Ok(md_files)
}

fn create_index_html(md_files: &[MarkdownFile], output_dir: &Path) -> io::Result<()> {
    let mut file_links = String::new();
    let mut files_by_directory: HashMap<String, Vec<&MarkdownFile>> = HashMap::new();

    // Group files by directory
    for file in md_files {
        let parent = file
            .path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("Root");

        files_by_directory
            .entry(parent.to_string())
            .or_default()
            .push(file);
    }

    // Generate HTML for each directory group
    for (_, files) in files_by_directory.iter() {
        file_links.push_str("<h2>Container runtime from scratch</h2>\n<ul>\n");

        for file in files {
            let rel_path = file.html_path.strip_prefix(output_dir).unwrap();
            file_links.push_str(&format!(
                r#"<li><a href="/{}">{}</a></li>"#,
                rel_path.display(),
                file.title
            ));
        }

        file_links.push_str("</ul>\n");
    }

    let html_content = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Container runtime from scratch</title>
    <link rel="stylesheet" href="/static/style.css">
</head>
<body>
    <header>
        <h1>Container runtime from scratch</h1>
    </header>
    <main>
        <div class="file-list">
            {}
        </div>
    </main>
    <footer>
        <p>Generated with love</p>
    </footer>
</body>
</html>"#,
        file_links
    );

    let index_path = output_dir.join("index.html");
    let mut file = File::create(index_path)?;
    file.write_all(html_content.as_bytes())?;

    Ok(())
}

fn create_css_file(static_dir: &Path) -> io::Result<()> {
    // Write CSS file
    let css_content = include_str!("style.css");

    let css_path = static_dir.join("style.css");
    let mut file = File::create(css_path)?;
    file.write_all(css_content.as_bytes())?;

    // Add Prism.js for syntax highlighting
    let prism_js_content = include_prism_js();
    let prism_js_path = static_dir.join("prism.js");
    let mut file = File::create(prism_js_path)?;
    file.write_all(prism_js_content.as_bytes())?;

    let prism_css_content = include_prism_css();
    let prism_css_path = static_dir.join("prism.css");
    let mut file = File::create(prism_css_path)?;
    file.write_all(prism_css_content.as_bytes())?;

    Ok(())
}

fn syntax_highlight_code_blocks(html: &str) -> String {
    // Very basic implementation that adds Prism.js classes to code blocks
    // A more robust implementation would use a proper HTML parser

    let mut result = html.to_string();

    // Replace <pre><code> with <pre><code class="language-xxx">
    // This is a simplistic approach - a real implementation would be more robust
    let code_block_pattern = r#"<pre><code>"#;
    let code_block_replacement = r#"<pre><code class="language-rust">"#;

    result = result.replace(code_block_pattern, code_block_replacement);

    // Look for language indicators like ```rust and add appropriate class
    let lang_indicators = [
        ("```rust", "language-rust"),
        ("```js", "language-javascript"),
        ("```javascript", "language-javascript"),
        ("```python", "language-python"),
        ("```html", "language-html"),
        ("```css", "language-css"),
        ("```bash", "language-bash"),
        ("```shell", "language-bash"),
        ("```c", "language-c"),
        ("```cpp", "language-cpp"),
        ("```java", "language-java"),
        ("```go", "language-go"),
    ];

    // This is a very crude implementation - in a real app you'd want to use a proper parser
    for (indicator, class) in lang_indicators.iter() {
        result = result.replace(
            &format!(r#"<pre><code class="language-rust">{}"#, indicator),
            &format!(r#"<pre><code class="{}">"#, class),
        );
    }

    result
}

fn include_prism_js() -> &'static str {
    include_str!("prism.js")
}

fn include_prism_css() -> &'static str {
    include_str!("prism.css")
}
