//! Run the extractor over a real log: `cargo run -p ap-build --example extract -- LOG SDIST_ROOT`
use ap_build::log::{PathMapper, extract};

fn main() -> color_eyre::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: extract LOG [SDIST_ROOT]");
    let raw = std::fs::read_to_string(&path)?;
    let mapper = match args.next() {
        Some(root) => PathMapper::with_sdist_root(root),
        None => PathMapper::default(),
    };

    let e = extract(&raw, &mapper);
    let raw_tokens = raw.len() / 4;
    let out_tokens = e.excerpt.len() / 4;

    println!("raw:      {} bytes (~{raw_tokens} tokens)", raw.len());
    println!(
        "excerpt:  {} bytes (~{out_tokens} tokens), {} lines",
        e.excerpt.len(),
        e.excerpt.lines().count()
    );
    println!(
        "reduction: {:.0}x",
        raw.len() as f64 / e.excerpt.len().max(1) as f64
    );
    println!("section:  {:?}", e.failed_section);
    println!("suspect:  {}", e.format_suspect);
    println!("frames:   {}", e.python_frames.len());
    println!("errors:   {:?}", e.error_lines);
    println!("primary:  {:?}", e.primary_source_file());
    println!(
        "source:   {:?}   (level 3, on demand)",
        e.source_candidates(8)
    );
    println!(
        "installed:{:?}   (level 4, last resort)",
        e.installed_candidates(4)
    );
    println!("\n--- excerpt ---\n{}", e.excerpt);
    Ok(())
}
