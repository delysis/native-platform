use loom_signal_protocol::{Command, MAX_FRAME_BYTES, Request, read_frame, write_frame};

#[tokio::test]
async fn fragmented_frames_preserve_unicode_and_eof_is_distinct_from_truncation() {
    let request = Request {
        id: "message-1".into(),
        command: Command::Send {
            conversation_id: "friend".into(),
            text: "  Hello 🌻\r\n".into(),
            timestamp: 42,
        },
    };
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &request).await.unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(1);
    let task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        writer.write_all(&bytes).await.unwrap();
    });
    let decoded: Request = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(decoded.command, Command::Send { text, .. } if text == "  Hello 🌻\r\n"));
    assert!(read_frame::<Request>(&mut reader).await.unwrap().is_none());
    task.await.unwrap();
    assert!(read_frame::<Request>(&mut &b"\0\0"[..]).await.is_err());
}

#[tokio::test]
async fn oversized_and_unknown_commands_are_rejected_before_dispatch() {
    let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
    assert!(read_frame::<Request>(&mut &prefix[..]).await.is_err());
    let mut frame = Vec::new();
    write_frame(
        &mut frame,
        &serde_json::json!({ "id": "one", "command": { "kind": "execute", "code": "untrusted" } }),
    )
    .await
    .unwrap();
    assert!(read_frame::<Request>(&mut frame.as_slice()).await.is_err());
}
