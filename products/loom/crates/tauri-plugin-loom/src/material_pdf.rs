//! Original PDF pages are ephemeral pixels, never replacement source evidence.
use super::*;
use base64::Engine as _;
use hayro::{RenderCache, RenderSettings, hayro_interpret::InterpreterSettings, hayro_syntax::Pdf};

#[derive(Debug, serde::Serialize)]
pub(crate) struct PdfPage {
    page: usize,
    page_count: usize,
    width: u16,
    height: u16,
    png_base64: String,
    incomplete: bool,
}

fn unavailable() -> IpcFailure {
    IpcFailure::new(
        "pdf_unavailable",
        "This retained PDF is no longer available in this workspace.",
        false,
    )
}

fn render_page(bytes: Vec<u8>, number: usize) -> Result<PdfPage, IpcFailure> {
    let pdf = Pdf::new(bytes).map_err(|_| {
        IpcFailure::new(
            "pdf_invalid",
            "The original PDF could not be opened.",
            false,
        )
    })?;
    if pdf.pages().len() > 10_000 {
        return Err(IpcFailure::new(
            "pdf_page_limit",
            "This PDF has too many pages to preview here. The original is retained.",
            false,
        ));
    }
    let page = number
        .checked_sub(1)
        .and_then(|index| pdf.pages().get(index))
        .ok_or_else(|| IpcFailure::new("pdf_page_missing", "This PDF has no such page.", false))?;
    let (width, height) = page.render_dimensions();
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(IpcFailure::new(
            "pdf_page_invalid",
            "This PDF page has invalid dimensions.",
            false,
        ));
    }
    // At most four million output pixels. This bounds the bitmap, not the
    // interpreter's intermediate allocations or time on a complex source.
    let scale = (2048.0 / width.max(height)).min(2.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (width, height) = (
        (width * scale).ceil().clamp(1.0, 2048.0) as u16,
        (height * scale).ceil().clamp(1.0, 2048.0) as u16,
    );
    let incomplete = Arc::new(AtomicBool::new(false));
    let warned = Arc::clone(&incomplete);
    let interpreter = InterpreterSettings {
        warning_sink: Arc::new(move |_| {
            warned.store(true, Ordering::Relaxed);
        }),
        ..InterpreterSettings::default()
    };
    let settings = RenderSettings {
        x_scale: scale,
        y_scale: scale,
        width: Some(width),
        height: Some(height),
        bg_color: hayro::vello_cpu::color::palette::css::WHITE,
    };
    let png = hayro::render(page, &RenderCache::new(), &interpreter, &settings)
        .into_png()
        .map_err(|_| {
            IpcFailure::new(
                "pdf_render_failed",
                "This PDF page could not be drawn.",
                false,
            )
        })?;
    Ok(PdfPage {
        page: number,
        page_count: pdf.pages().len(),
        width,
        height,
        png_base64: base64::engine::general_purpose::STANDARD.encode(png),
        incomplete: incomplete.load(Ordering::Relaxed),
    })
}

async fn read_page(
    state: &PluginState,
    request: &Request,
    page: usize,
    operation_id: &str,
) -> Result<PdfPage, IpcFailure> {
    let authority = capture_loom_asset_authority_for(state, request.project_id, request.session_id)
        .map_err(|_| unavailable())?;
    let attachment = selected_attachment(state, &authority, request).map_err(|_| unavailable())?;
    if attachment != request.media_sha256 {
        return Err(unavailable());
    }
    let operation = import_jobs::ImportOperation::reserve_in(
        state,
        &state.previews,
        &request.project_id.to_string(),
        &request.session_id.to_string(),
        operation_id,
    )?;
    let root = operation.root.clone();
    let id = attachment.clone();
    let rendered = operation
        .compute(move || {
            let bytes =
                context_attachments::read_pdf_original(&root, &id).map_err(|_| unavailable())?;
            render_page(bytes, page)
        })
        .await?;
    // Publication checks the live root identity and binding under the same
    // admission lock as removal and close. Rendering and original-byte reads
    // never hold that lock.
    operation.publish_to_store(state, |store| {
        if attachment_in_store(store, request).map_err(|_| unavailable())? != attachment {
            return Err(unavailable());
        }
        Ok(rendered)
    })
}

#[tauri::command]
pub(crate) async fn material_pdf_page(
    token: String,
    page: usize,
    operation_id: String,
    state: State<'_, PluginState>,
) -> Result<PdfPage, IpcFailure> {
    let uri = format!("loom-asset://localhost/{token}")
        .parse()
        .map_err(|_| unavailable())?;
    let request = parse(&uri).ok_or_else(unavailable)?;
    read_page(&state, &request, page, &operation_id).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    const PDF: &[u8] = include_bytes!("../tests/fixtures/original-pages.pdf");

    #[test]
    fn original_pages_render_distinct_graphics_and_text() {
        for (page, color) in [(1, [255, 0, 0, 255]), (2, [0, 0, 255, 255])] {
            let rendered = render_page(PDF.to_vec(), page).unwrap();
            assert_eq!(
                (
                    rendered.page,
                    rendered.page_count,
                    rendered.width,
                    rendered.height
                ),
                (page, 2, 800, 400)
            );
            let png = base64::engine::general_purpose::STANDARD
                .decode(rendered.png_base64)
                .unwrap();
            let image = image::load_from_memory(&png).unwrap().into_rgba8();
            assert_eq!(image.get_pixel(80, 320).0, color);
            assert!(
                image
                    .enumerate_pixels()
                    .any(|(x, y, pixel)| x > 80 && y < 170 && pixel.0[..3] == [0, 0, 0]),
                "original text must be drawn as well as graphics"
            );
        }
        assert_eq!(
            render_page(PDF.to_vec(), 0).unwrap_err().code,
            "pdf_page_missing"
        );
        assert_eq!(
            render_page(PDF.to_vec(), 3).unwrap_err().code,
            "pdf_page_missing"
        );
        assert!(render_page(b"not a PDF".to_vec(), 1).is_err());
    }

    #[test]
    fn retained_pdf_preview_obeys_binding_and_session_authority() {
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(temp.path().join("Writing"), "Writing").unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/original-pages.pdf");
        let attachment = context_attachments::import_path(store.root(), &path).unwrap();
        let entry = materials::bind_attachment(&mut store, &attachment.id, None).unwrap();
        let project = store.manifest().project_id;
        let session_id = CommandId::new();
        let source = bind_tokens(
            materials::read(&store, &entry.id).unwrap(),
            &project.to_string(),
            &session_id.to_string(),
        )
        .unwrap();
        let token = source.presentation.unwrap().pdf_preview_token.unwrap();
        let request = parse(&format!("loom-asset://localhost/{token}").parse().unwrap()).unwrap();
        let state = PluginState::default();
        {
            let mut session = state.session.lock().unwrap();
            session.phase = SessionPhase::Open;
            session.active_session_id = Some(session_id);
            session.store = Some(store);
        }
        let preview = |request: &Request| {
            tauri::async_runtime::block_on(read_page(
                &state,
                request,
                2,
                &CommandId::new().to_string(),
            ))
        };
        assert_eq!(preview(&request).unwrap().page, 2);
        assert!(
            preview(&Request {
                session_id: CommandId::new(),
                ..request.clone()
            })
            .is_err()
        );
        assert!(
            preview(&Request {
                media_sha256: "0".repeat(64),
                ..request.clone()
            })
            .is_err()
        );
        {
            let mut session = state.session.lock().unwrap();
            materials::remove(session.store.as_mut().unwrap(), &entry.id).unwrap();
        }
        assert!(preview(&request).is_err());
        assert_eq!(std::fs::read(path).unwrap(), PDF);
    }
}
