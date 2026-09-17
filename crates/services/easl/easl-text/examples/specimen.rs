//! Offscreen native glyphs composed by the EASL paragraph library.
use easl_native_text::{Alignment, RasterSurface, TextStyle, TextSystem, WhiteSpace};
use easl_text::{Composer, Policy};
use std::{error::Error, path::PathBuf, time::Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from("target/easl-library-specimen.png"),
        PathBuf::from,
    );
    let started = Instant::now();
    let mut composer = Composer::new()?;
    let compile_ms = started.elapsed().as_secs_f64() * 1000.;
    let mut system = TextSystem::new();
    let mut surface = RasterSurface::new(1600, 1260, 2.)?;
    surface.begin(1600, 1260, 2.)?;
    surface.rect([0., 0., 800., 630.], [249, 246, 238, 255])?;
    let body = TextStyle {
        family: "Baskerville, Georgia, serif".into(),
        size: 20.,
        line_height: 30.,
        color: [43, 49, 43, 255],
        ..TextStyle::default()
    };
    let title = TextStyle {
        size: 38.,
        line_height: 48.,
        ..body.clone()
    };
    let mut heading = system.prepare(
        "The language chooses the lines.",
        &title,
        WhiteSpace::Preserve,
        &[],
        &[],
    )?;
    composer.compose(&mut heading, 704., Alignment::Start, Policy::default())?;
    surface.text(heading.layout(), [48., 40.], [48., 40., 704., 112.])?;
    let source = "A page begins with proportion: the width of a line, the space between its neighbours, and the small intervals that let one word meet the next. Good typography gives these relationships a quiet order. Here the EASL library considers possible endings across the whole paragraph. Native shaping supplies the measurements; the language chooses the composition.";
    let mut prepared = system.prepare(source, &body, WhiteSpace::Preserve, &[], &[])?;
    for (x, weight) in [(48., 0.), (424., 1.)] {
        composer.compose(
            &mut prepared,
            328.,
            Alignment::Start,
            Policy {
                last_line_weight: weight,
                ..Policy::default()
            },
        )?;
        surface.text(prepared.layout(), [x, 138.], [x, 138., 328., 455.])?;
    }
    surface.finish();
    std::fs::write(&output, surface.png()?)?;
    println!("specimen={} compile_ms={compile_ms:.3}", output.display());

    for repetitions in [1, 8, 32] {
        let source = source.repeat(repetitions);
        let start = Instant::now();
        let mut prepared = system.prepare(&source, &body, WhiteSpace::Preserve, &[], &[])?;
        let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
        let preparations = system.preparations;
        let mut samples = Vec::new();
        let mut native_samples = Vec::new();
        for pass in 0..101 {
            let width = if pass % 2 == 0 { 328. } else { 440. };
            let start = Instant::now();
            composer.compose(&mut prepared, width, Alignment::Start, Policy::default())?;
            let elapsed = start.elapsed().as_secs_f64() * 1000.;
            if pass != 0 {
                samples.push(elapsed);
            }
            let start = Instant::now();
            prepared.optimize(width, Alignment::Start)?;
            if pass != 0 {
                native_samples.push(start.elapsed().as_secs_f64() * 1000.);
            }
        }
        samples.sort_by(f64::total_cmp);
        native_samples.sort_by(f64::total_cmp);
        assert_eq!(system.preparations, preparations);
        println!(
            "source_bytes={} prepare_ms={prepare_ms:.3} native_plus_easl_reflow_p50_ms={:.3} p95_ms={:.3} samples=100",
            source.len(),
            samples[49],
            samples[94]
        );
        println!(
            "source_bytes={} native_only_reflow_p50_ms={:.3} p95_ms={:.3} samples=100",
            source.len(),
            native_samples[49],
            native_samples[94]
        );
    }
    Ok(())
}
