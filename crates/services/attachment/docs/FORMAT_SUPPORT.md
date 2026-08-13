# Format support and evidence

Support is reported at four separate evidence levels:

1. **Detected**: bytes contain a stable signature or pass a structural probe.
2. **Inspected**: bounded metadata or children were read without executing content.
3. **Canonicalized**: useful content was converted to a versioned Markdown or
   plain-text artifact with source provenance.
4. **Direct media**: validated original bytes may be offered only to a target
   that advertises the exact media type/family and sufficient byte/object limits.

An extension or declared media type is only a hint. It never grants one of
these evidence levels by itself.

## In-process support matrix

| Family | Formats | Current evidence | Important boundary |
|---|---|---|---|
| Text | UTF-8 text, Markdown, WebVTT, SubRip | detected, canonicalized | UTF-8 byte ranges and global text limit |
| Structured text | JSON, CSV, TSV, XML, SVG, Jupyter notebooks | structurally validated, canonicalized | no entity resolution, script execution, or external fetch |
| HTML | HTML | structurally converted to Markdown | Cloudflare-derived `html-to-markdown-rs`; links remain text and resources are not fetched |
| Email | RFC 822/MIME | recursively inspected, headers/body canonicalized | parts re-enter the same global object queue; names/MIME are untrusted |
| PDF | PDF text and embedded files | strictly loaded, active-content names reported, text canonicalized, embedded files recursively inspected | encrypted PDFs unsupported; no JavaScript, launch action, rich-media execution, or page rasterization |
| OOXML | DOCX, PPTX, XLSX | ZIP structure inspected and canonicalized from inspected child parts | macros are never run; malformed parts are explicit partial results |
| Publications | EPUB | container/spine inspected and canonicalized | archive relationships cannot escape the content-addressed namespace |
| OpenDocument | ODT, ODS, ODP | structure inspected and `content.xml` canonicalized | spreadsheet structure is intentionally conservative; active content is reported |
| Apple iWork | Pages, Numbers, Keynote | container members recursively inspected | `.iwa` protobuf content is not canonicalized yet |
| Legacy Office | OLE compound files | content-first detected | no in-process compound-document parser; typed unsupported/opaque result |
| Archives | ZIP, TAR | bounded recursive inspection | names are inert; no links, devices, sparse materialization, or filesystem extraction |
| 7z | one-folder/one-pack LZMA/LZMA2 subset accepted by `libarchive_oxide 0.2.0` | bounded recursive inspection | encryption, unsupported coders/layouts, or native-limit failures are typed; compressed-size ratio and independent CRC fields are not exposed by this reader |
| Stream compression | GZIP, BZIP2, XZ, Zstandard | bounded decode then recursive inspection | XZ block dictionaries and Zstd windows are pre-limited independently of output; concatenated XZ streams are unsupported in process |
| RAR | RAR 4/5 signatures | detected | deliberately unsupported in process; no audited decoder exposes the required memory/step limits |
| Raster images | PNG, JPEG, GIF, WebP, BMP, TIFF, HEIF, AVIF | PNG/JPEG receive a bounded complete payload decode; the remaining formats receive a structural/dimension probe | only payload-decoded media may be direct; structure-only formats require an explicit transform or remain blocked even when the target names that media type |
| Vector image | SVG | XML canonicalization; direct when target allows | active/external content is data only and never fetched or executed |
| Audio | WAV, AIFF, CAF, FLAC, MP3, Opus/Vorbis/Speex/FLAC-in-Ogg, M4A | container/frame probe; direct when target allows, otherwise explicit transcription request | ambiguous Ogg remains generic; core does not decode or transcribe |
| Video | MP4, QuickTime, Theora-in-Ogg, Matroska, WebM, AVI | container probe; direct when target allows, otherwise explicit frame/audio DAG | core does not demux, decode frames, or invoke codecs |
| Executables | common executable signatures | detected and blocked | never canonicalized or offered as opaque content by the default policy |
| Unknown binary | anything not proven above | explicit opaque or blocked result | policy-controlled; never treated as text or clean content |

RTF is content-first detected, but remains opaque because a maintained,
bounded safe-Rust canonicalizer has not yet met this repository's bar.

## Transform-needed formats

Planning may emit a typed, dependency-ordered request for OCR, transcription,
video audio extraction, video frame sampling, PDF rasterization, or another
document extractor. A request is not evidence the transform ran. The embedding
application must inject an adapter, enforce its limits, and return a receipt.

`speech-native-kit` is the intended local transcription owner. A separate
killable media worker is the intended owner for general video/PDF raster work.
Remote fallback never happens automatically.

## Decoder policy

The default in-process lane favors maintained safe-Rust parsers with explicit
resource controls. First-party crates forbid unsafe code. Some audited
transitive parsers may contain unsafe implementation details; the repository
does not falsely claim the entire dependency graph is unsafe-free.

Comprehensive RAR, exotic 7z coders, multipart archive sets, hard-deadline
media decoders, and heavyweight legacy-office conversion belong behind an
optional killable worker protocol. Such a worker must receive bytes, not an
ambient path; expose entry and resource events; and remain unable to write into
the application's data tree directly.
