//! Synthetic, offscreen editor timings. Run with `--profile native-view`.
//! Reports cold layout separately from movement and reflow on unchanged text.
use easl_native_text::{EditCommand, Movement, TextEditor, TextStyle, TextSystem};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut system = TextSystem::new();
    for repeats in [100, 1_000, 5_000] {
        let text = "A café with e\u{301}, words and space.\n".repeat(repeats);
        let style = TextStyle::default();
        let start = Instant::now();
        let mut editor = TextEditor::new(&text, style.clone())?;
        editor.ensure_layout(&mut system, &style, 720., 500.)?;
        let layout = start.elapsed();
        let mut samples = Vec::with_capacity(1_000);
        for _ in 0..1_000 {
            let start = Instant::now();
            editor.command(&mut system, EditCommand::Move(Movement::Right, false))?;
            samples.push(start.elapsed().as_nanos());
        }
        samples.sort_unstable();
        let mut resize = Vec::with_capacity(100);
        let shaping = editor.inner().shaping_generation();
        for i in 0..100 {
            let start = Instant::now();
            editor.ensure_layout(
                &mut system,
                &style,
                if i % 2 == 0 { 240. } else { 720. },
                500.,
            )?;
            resize.push(start.elapsed().as_nanos());
        }
        resize.sort_unstable();
        println!(
            "bytes={} layout_ms={:.3} move_median_us={:.3} move_p95_us={:.3} resize_median_ms={:.3} resize_p95_ms={:.3} shape_reused={}",
            text.len(),
            layout.as_secs_f64() * 1_000.,
            samples[500] as f64 / 1_000.,
            samples[950] as f64 / 1_000.,
            resize[50] as f64 / 1_000_000.,
            resize[95] as f64 / 1_000_000.,
            shaping == editor.inner().shaping_generation(),
        );
        assert!(editor.equals(&text));
    }
    Ok(())
}
