//! Offline diff-format workbench using the same public Rust comparison API.
//! Run with OUTPUT.html, optionally followed by BEFORE.txt and AFTER.txt.
//! Inputs are exact UTF-8 snapshots; no Google account or model is contacted.
use std::{
    fmt::Write as _,
    fs::File,
    io::{Read as _, Write as _},
    path::Path,
};

use loom_document::revision_diff::{ChangeKind, Granularity, MAX_INPUT_BYTES, compare};
use sha2::{Digest as _, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Case {
    name: String,
    question: String,
    before: String,
    after: String,
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 && args.len() != 3 {
        return Err("Usage: revision_workbench OUTPUT.html [BEFORE.txt AFTER.txt]".into());
    }
    let cases = if args.len() == 3 {
        vec![Case {
            name: "Your two snapshots".into(),
            question: "What changed? Is it factual, editorial, structural, or formatting? The diff does not infer the writer's reason.".into(),
            before: read_text(Path::new(&args[1]))?,
            after: read_text(Path::new(&args[2]))?,
        }]
    } else {
        fixtures()
    };
    let mut html = String::from(HEADER);
    for (index, case) in cases.iter().enumerate() {
        render_case(&mut html, index + 1, case)?;
    }
    html.push_str("</main></body></html>");
    let mut output = File::create_new(&args[0])?;
    output.write_all(html.as_bytes())?;
    output.sync_all()?;
    println!(
        "Wrote {} comparisons to {}",
        cases.len(),
        Path::new(&args[0]).display()
    );
    Ok(())
}

fn read_text(path: &Path) -> Result<String> {
    let mut text = String::new();
    File::open(path)?
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > MAX_INPUT_BYTES {
        return Err("snapshot exceeds the input limit".into());
    }
    Ok(text)
}

fn render_case(html: &mut String, number: usize, case: &Case) -> Result<()> {
    write!(
        html,
        "<article><h2>{number}. {}</h2><p class=question>{}</p>",
        escape(&case.name),
        escape(&case.question)
    )?;
    for (mode, class) in [(Granularity::Words, "words"), (Granularity::Lines, "lines")] {
        let diff = compare(&case.before, &case.after, mode)?;
        let (mut inline, mut before, mut after) = (String::new(), String::new(), String::new());
        for span in &diff.spans {
            let old = escape(&case.before[span.before.clone()]);
            let new = escape(&case.after[span.after.clone()]);
            match span.kind {
                ChangeKind::Equal => {
                    inline.push_str(&old);
                    before.push_str(&old);
                    after.push_str(&new);
                }
                ChangeKind::Insert | ChangeKind::Delete | ChangeKind::Replace => {
                    if !old.is_empty() {
                        write!(inline, "<del>{old}</del>")?;
                        write!(before, "<del>{old}</del>")?;
                    }
                    if !new.is_empty() {
                        write!(inline, "<ins>{new}</ins>")?;
                        write!(after, "<ins>{new}</ins>")?;
                    }
                }
            }
        }
        write!(
            html,
            "<section class={class}><div class=inline><div class=prose>{inline}</div></div><div class=split><section><h3>Before</h3><div class=prose>{before}</div></section><section><h3>After</h3><div class=prose>{after}</div></section></div>"
        )?;
        if diff.coarse_blocks > 0 {
            html.push_str("<p class=notice>Some blocks are shown as whole replacements because finer alignment exceeded the comparison budget.</p>");
        }
        write!(
            html,
            "<details><summary>Exact ranges · {mode:?}</summary><pre>{}</pre></details></section>",
            escape(&serde_json::to_string_pretty(&diff)?)
        )?;
    }
    write!(
        html,
        "<details><summary>Snapshot identity and exact whitespace</summary><p class=identity>Before SHA-256: {:x}<br>After SHA-256: {:x}</p><h3>Before</h3><pre>{}</pre><h3>After</h3><pre>{}</pre><p>Whitespace key: · space, → tab, ␍ carriage return, ␊ newline. Text is preserved; these markers are a display aid.</p></details></article>",
        Sha256::digest(case.before.as_bytes()),
        Sha256::digest(case.after.as_bytes()),
        escape(&visible_whitespace(&case.before)),
        escape(&visible_whitespace(&case.after))
    )?;
    Ok(())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn visible_whitespace(text: &str) -> String {
    text.replace(' ', "·")
        .replace('\t', "→")
        .replace('\r', "␍")
        .replace('\n', "␊\n")
}

fn fixtures() -> Vec<Case> {
    [
        ("A small edit with factual weight", "How many people did the pilot serve in each version? A small diff can change a major claim.", "The pilot helped 12 people find stable housing.\n", "The pilot helped 21 people find stable housing.\n"),
        ("A writer removes an unsupported claim", "Which promise disappeared? Do not infer whether the writer disliked the tone or lacked evidence.", "Our proven approach eliminates homelessness. We offer housing advice and follow-up visits.\n", "We offer housing advice and follow-up visits.\n"),
        ("Voice and specificity", "Which version would you use, and in what setting? A preference needs its audience and purpose.", "We leverage innovative partnerships to drive transformative impact.\n", "We work with local shelters to help people find a home.\n"),
        ("Reordered paragraphs", "Did any wording change, or only the order? This prototype reports exact insertion/deletion without asserting a move.", "The team listens first.\n\nThen we make a plan.\n\nResidents choose the next step.\n", "Residents choose the next step.\n\nThe team listens first.\n\nThen we make a plan.\n"),
        ("A citation changes under the same label", "The visible link label is unchanged. Which source URL changed? A rendered-text-only diff would miss this.", "The survey found a decline. [Source](https://example.test/2024)\n", "The survey found a decline. [Source](https://example.test/2025)\n"),
        ("Verse, indentation, and line endings", "Where did whitespace change? Try Lines and the exact-whitespace view. These bytes are part of the work.", "  hold the light\r\n\tuntil morning  \r\n", " hold the light\n\tuntil morning\n"),
        ("Unicode and a literal untrusted tag", "Do highlights preserve the accent and emoji? Literal markup must stay inert.", "Cafe\u{301} 👩🏽‍💻 writes <script>alert('x')</script>.\n", "Café 👨🏽‍💻 revises <script>alert('x')</script>.\n"),
        ("A preference reverses", "A later revision is not automatically a better one. Ask the writer whether this restores meaning or merely changes phrasing.", "Perhaps the pilot may help people.\n", "The pilot helps people.\n"),
    ].into_iter().map(|(name, question, before, after)| Case { name: name.into(), question: question.into(), before: before.into(), after: after.into() }).collect()
}

const HEADER: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'"><title>Loom · Revision comparisons</title><style>
:root{color-scheme:light;--ink:#242e2b;--paper:#fbfaf6;--line:#d8ddd6;--muted:#5c6963}*{box-sizing:border-box}body{margin:0 auto;padding:30px clamp(20px,4vw,64px);max-width:1260px;background:var(--paper);color:var(--ink);font:16px/1.55 system-ui,sans-serif}h1{font:500 34px/1.2 Georgia,serif;margin:0 0 10px}h2{font-size:19px;margin:0}h3{font-size:13px;color:var(--muted);margin:0 0 12px;text-transform:uppercase;letter-spacing:.05em}header p{max-width:78ch;margin:8px 0;color:var(--muted)}.eyebrow{font-size:12px;letter-spacing:.13em;text-transform:uppercase}.controls{display:flex;gap:14px;flex-wrap:wrap;border-top:1px solid var(--line);border-bottom:1px solid var(--line);padding:16px 0;margin:24px 0 0}.controls label{cursor:pointer;padding:7px 12px;border-radius:5px;border:1px solid var(--line)}.controls span{align-self:center;color:var(--muted);font-size:13px}body>input{position:absolute;opacity:0;pointer-events:none}#inline:checked~.controls label[for=inline],#split:checked~.controls label[for=split],#words:checked~.controls label[for=words],#lines:checked~.controls label[for=lines]{background:#253c32;color:white}body>input:focus-visible~.controls{outline:3px solid #5b7cba;outline-offset:3px}article{padding:28px 0;border-bottom:1px solid var(--line)}.question{margin:7px 0 20px;color:var(--muted);max-width:90ch;font-size:14px}.prose{white-space:pre-wrap;overflow-wrap:anywhere;font:21px/1.8 Georgia,serif;tab-size:4;min-height:36px}.inline{max-width:85ch}.split{display:grid;grid-template-columns:1fr 1fr;gap:28px}.split>section+section{border-left:1px solid var(--line);padding-left:28px}del{background:#f7e2d6;color:#783919;text-decoration:line-through;text-decoration-thickness:1px}ins{background:#dcebdd;color:#174c2e;text-decoration:underline;text-underline-offset:3px}.legend{font-size:13px;margin:12px 0;color:var(--muted)}.legend del,.legend ins{padding:2px 5px}.notice{color:#783919;font-size:14px}details{margin-top:18px;font-size:13px}summary{cursor:pointer;color:var(--muted);padding:4px 0}pre{font:12px/1.55 ui-monospace,monospace;white-space:pre-wrap;overflow-wrap:anywhere;background:#eeeee8;padding:14px;border-radius:5px;max-height:400px;overflow:auto}.identity{font:11px/1.8 ui-monospace,monospace;overflow-wrap:anywhere}#inline:checked~main .split,#split:checked~main .inline,#words:checked~main .lines,#lines:checked~main .words{display:none}@media(max-width:680px){.split{grid-template-columns:1fr;gap:20px}.split>section+section{border-left:0;border-top:1px solid var(--line);padding:16px 0 0}.prose{font-size:19px}}@media print{body{padding:0}.controls{display:none}}
</style></head><body><header><p class=eyebrow>Loom · Format study · Synthetic examples</p><h1>Read the revision.</h1><p>Compare the same edits in two layouts and two levels of detail. Judge each by whether you can explain the change and catch a factual error. These are exact text comparisons, not inferred authorship or a learned preference.</p><p>This is an offline prototype, not Loom's installed history UI. No accounts, model calls, telemetry, or saved ratings.</p></header><input type=radio name=layout id=inline checked><input type=radio name=layout id=split><input type=radio name=granularity id=words checked><input type=radio name=granularity id=lines><div class=controls role=group aria-label="Comparison format"><span>Layout</span><label for=inline>Inline</label><label for=split>Before / after</label><span>Detail</span><label for=words>Words</label><label for=lines>Lines</label></div><p class=legend><del>Deleted text</del> <ins>Inserted text</ins> · Unmarked text is unchanged. Source syntax and whitespace are preserved.</p><main>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_source_stays_text_in_every_view() {
        let mut html = String::new();
        let case = Case {
            name: "<img src=x>".into(),
            question: "&".into(),
            before: "<script>x()</script>".into(),
            after: "<script>y()</script>".into(),
        };
        render_case(&mut html, 1, &case).unwrap();
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img src="));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("Snapshot identity"));
    }
}
