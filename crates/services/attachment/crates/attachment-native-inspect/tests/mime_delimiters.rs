use attachment_native_inspect::{Inspector, ProvidedAttachment};
use attachment_native_types::{
    AttachmentBundle, BudgetLimits, Coverage, DetectedFormat, EdgeOutcome, InspectionPolicy,
    IssueClass, IssueSeverity, ObjectStatus,
};
use mail_parser::{Encoding, MessageParser, PartType};
use std::fmt::Write;

// The first five values are legal RFC 2046 boundary values. Later cases cover
// the locked parser's accepted folded/encoded/escaped parameter language.
const LEGAL: [(&str, &str); 5] = [
    ("simple", "multipart/mixed; boundary=simple"),
    ("with space", "multipart/mixed; boundary=\"with space\""),
    (" leading", "multipart/mixed; boundary=\" leading\""),
    ("simple--", "multipart/mixed; boundary=\"simple--\""),
    (
        " leading space--",
        "multipart/mixed; boundary=\" leading space--\"",
    ),
];
const DECODED: [(&str, &str); 5] = [
    (
        "folded space--",
        "multipart/mixed;\r\n\tboundary=\"folded space--\"",
    ),
    (
        "encoded boundary--",
        "multipart/mixed; boundary*=utf-8''encoded%20boundary--",
    ),
    (
        "continued part--",
        "multipart/mixed; boundary*0=\"continued \"; boundary*1=\"part--\"",
    ),
    (
        "word boundary--",
        "multipart/mixed; boundary=\"=?utf-8?Q?word_boundary--?=\"",
    ),
    (
        "escaped space--",
        "multipart/mixed; boundary=\"escaped\\ space--\"",
    ),
];

fn root_headers(content_type: &str) -> String {
    format!(
        "From: source@example.test\r\nSubject: MIME admission\r\nMIME-Version: 1.0\r\nContent-Type: {content_type}\r\n\r\n"
    )
}

fn part_headers(index: usize) -> String {
    format!(
        "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=\"part-{index}.txt\"\r\n\r\n"
    )
}

fn multipart(boundary: &str, content_type: &str, count: usize) -> Vec<u8> {
    let mut message = root_headers(content_type);
    for index in 0..count {
        write!(
            message,
            "--{boundary}\r\n{}part {index}\r\n",
            part_headers(index)
        )
        .expect("fixture");
    }
    write!(message, "--{boundary}--\r\n").expect("fixture");
    message.into_bytes()
}

fn policy(parts: u32) -> InspectionPolicy {
    InspectionPolicy {
        limits: BudgetLimits {
            max_entries: parts,
            max_edges: parts,
            max_objects: parts + 1,
            ..BudgetLimits::default()
        },
        ..InspectionPolicy::default()
    }
}

fn inspect(bytes: &[u8], policy: InspectionPolicy) -> AttachmentBundle {
    let bundle = Inspector::new(policy)
        .expect("fixture policy")
        .inspect(ProvidedAttachment::from_bytes(
            "witness.eml",
            Some("message/rfc822".to_string()),
            bytes,
        ))
        .expect("public inspection should produce a graph");
    bundle.validate().expect("public accounting contract");
    assert_eq!(
        bundle.graph.objects[0].detection.selected,
        Some(DetectedFormat::Email)
    );
    assert_eq!(
        bundle
            .blobs
            .get(&bundle.graph.root)
            .expect("retained root")
            .as_ref(),
        bytes
    );
    bundle
}

fn assert_blocked(bytes: &[u8], policy: InspectionPolicy, code: &str, class: IssueClass) {
    let bundle = inspect(bytes, policy);
    assert!(
        bundle.graph.issues.iter().any(|issue| issue.code == code
            && issue.class == class
            && issue.severity == IssueSeverity::Blocked),
        "{:#?}",
        bundle.graph
    );
    assert_eq!(bundle.graph.objects.len(), 1, "{:#?}", bundle.graph);
    assert!(bundle.graph.edges.is_empty(), "{:#?}", bundle.graph);
    assert_eq!(bundle.graph.usage.entries, 0);
    assert_eq!(bundle.graph.usage.total_derived_bytes, 0);
    assert!(matches!(bundle.graph.coverage, Coverage::Partial { .. }));
    assert!(
        matches!(&bundle.graph.objects[0].status, ObjectStatus::Partial { reasons } if reasons.iter().any(|reason| reason == code))
    );
    assert!(
        !bundle
            .graph
            .issues
            .iter()
            .any(|issue| issue.code == "archive_entry_limit_exceeded"
                || issue.class == IssueClass::Malformed),
        "{:#?}",
        bundle.graph
    );
}

fn assert_width_blocked(boundary: &str, content_type: &str) {
    let bytes = multipart(boundary, content_type, 2);
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("accepted fixture")
            .attachment_count(),
        2
    );
    assert_blocked(
        &bytes,
        policy(1),
        "mime_part_limit_exceeded",
        IssueClass::Budget,
    );
}

fn assert_leaf_parts(bytes: &[u8], policy: InspectionPolicy, count: usize) {
    let bundle = inspect(bytes, policy);
    assert_eq!(bundle.graph.edges.len(), count, "{:#?}", bundle.graph);
    assert_eq!(bundle.graph.usage.entries as usize, count);
    assert_eq!(bundle.graph.objects.len(), count + 1);
    assert!(
        bundle
            .graph
            .edges
            .iter()
            .all(|edge| edge.outcome == EdgeOutcome::Derived)
    );
    assert!(bundle.graph.issues.is_empty(), "{:#?}", bundle.graph);
    assert_eq!(bundle.graph.coverage, Coverage::Complete);
}

#[test]
fn mime_over_width_internal_space_is_blocked_before_materialization() {
    assert_width_blocked(LEGAL[1].0, LEGAL[1].1);
}

#[test]
fn mime_over_width_leading_space_is_blocked_before_materialization() {
    assert_width_blocked(LEGAL[2].0, LEGAL[2].1);
}

#[test]
fn mime_over_width_trailing_double_hyphen_is_blocked_before_materialization() {
    assert_width_blocked(LEGAL[3].0, LEGAL[3].1);
}

#[test]
fn mime_over_width_combined_space_and_suffix_is_blocked_before_materialization() {
    assert_width_blocked(LEGAL[4].0, LEGAL[4].1);
}

#[test]
fn mime_legal_exact_budget_parts_keep_useful_closing_markers() {
    for (boundary, content_type) in LEGAL {
        let bytes = multipart(boundary, content_type, 1);
        assert_eq!(
            MessageParser::default()
                .parse(&bytes)
                .expect("accepted fixture")
                .attachment_count(),
            1
        );
        assert_leaf_parts(&bytes, policy(1), 1);
    }
}

#[test]
fn mime_actual_header_decoding_keeps_exact_budget_controls() {
    for (boundary, content_type) in DECODED {
        let bytes = multipart(boundary, content_type, 1);
        assert_eq!(
            MessageParser::default()
                .parse(&bytes)
                .expect("accepted fixture")
                .attachment_count(),
            1,
            "{content_type}"
        );
        assert_leaf_parts(&bytes, policy(1), 1);
    }
}

#[test]
fn mime_over_width_actual_decoded_header_values_are_blocked() {
    for (boundary, content_type) in DECODED {
        assert_width_blocked(boundary, content_type);
    }
}

#[test]
fn mime_over_width_parser_permitted_embedded_delimiters_are_blocked() {
    let bytes = String::from_utf8(multipart(LEGAL[3].0, LEGAL[3].1, 2))
        .expect("fixture")
        .replace("--simple--", "padding--simple--")
        .into_bytes();
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("accepted fixture")
            .attachment_count(),
        2
    );
    assert_blocked(
        &bytes,
        policy(1),
        "mime_part_limit_exceeded",
        IssueClass::Budget,
    );
    let one = String::from_utf8(multipart(LEGAL[3].0, LEGAL[3].1, 1))
        .expect("fixture")
        .replace("--simple--", "padding--simple--")
        .into_bytes();
    assert_leaf_parts(&one, policy(1), 1);
}

#[test]
fn mime_active_boundary_is_the_last_parsed_content_type() {
    let bytes = String::from_utf8(multipart(LEGAL[4].0, LEGAL[4].1, 2))
        .expect("fixture")
        .replacen(
            "Content-Type: ",
            "Content-Type: multipart/mixed; boundary=decoy\r\nCoNTent - Type : ",
            1,
        )
        .into_bytes();
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("accepted fixture")
            .attachment_count(),
        2
    );
    assert_blocked(
        &bytes,
        policy(1),
        "mime_part_limit_exceeded",
        IssueClass::Budget,
    );
}

fn nested_multipart(outer: &str, inner: &str) -> Vec<u8> {
    let inner_message = multipart(inner, &format!("multipart/mixed; boundary=\"{inner}\""), 2);
    let mut bytes = root_headers(&format!("multipart/mixed; boundary=\"{outer}\""));
    write!(
        bytes,
        "--{outer}\r\n{}\r\n--{outer}--\r\n",
        String::from_utf8(inner_message).expect("fixture")
    )
    .expect("fixture");
    bytes.into_bytes()
}

#[test]
fn mime_over_width_nested_spaced_boundary_is_blocked() {
    let bytes = nested_multipart("outer space--", " inner space--");
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("accepted fixture")
            .attachment_count(),
        2
    );
    assert_blocked(
        &bytes,
        policy(1),
        "mime_part_limit_exceeded",
        IssueClass::Budget,
    );
}

#[test]
fn mime_nested_declared_boundaries_keep_structural_budget_controls() {
    for (outer, inner) in [
        ("outer", "inner"),
        ("outer space--", " inner space--"),
        ("outer", "outer--"),
        ("outer--", "outer"),
    ] {
        let bytes = nested_multipart(outer, inner);
        assert_eq!(
            MessageParser::default()
                .parse(&bytes)
                .expect("accepted fixture")
                .attachment_count(),
            2
        );
        // The established conservative budget includes container openings;
        // allow that structural part as well as the two retained leaves.
        assert_leaf_parts(&bytes, policy(3), 2);
    }
}

#[test]
fn mime_closed_epilogue_does_not_create_potential_children() {
    let mut bytes = multipart(LEGAL[0].0, LEGAL[0].1, 1);
    bytes.extend_from_slice(b"--unrelated\r\nContent-Disposition: attachment\r\n\r\nepilogue\r\n");
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("accepted fixture")
            .attachment_count(),
        1
    );
    assert_leaf_parts(&bytes, policy(1), 1);
}

#[test]
fn mime_undeclared_marker_text_remains_an_inline_body() {
    let bytes = format!("{}ordinary prose\r\n--x\r\nContent-Disposition: attachment\r\n\r\nfirst\r\n--x\r\nContent-Disposition: attachment\r\n\r\nsecond\r\n--x--\r\n", root_headers("text/plain")).into_bytes();
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("inline fixture")
            .attachment_count(),
        0
    );
    assert_leaf_parts(&bytes, policy(1), 0);
}

#[test]
fn mime_first_closing_match_keeps_locked_parser_recovery() {
    let bytes = format!("{}--simple----\r\n", root_headers(LEGAL[3].1)).into_bytes();
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("recoverable fixture")
            .attachment_count(),
        0
    );
    assert_leaf_parts(&bytes, policy(1), 0);
}

#[test]
fn mime_recovery_without_closing_markers_remains_useful_and_bounded() {
    let mut bytes = multipart(LEGAL[3].0, LEGAL[3].1, 1);
    bytes.truncate(bytes.len() - "--simple----\r\n".len());
    let parsed = MessageParser::default()
        .parse(&bytes)
        .expect("recoverable fixture");
    assert_eq!(parsed.attachment_count(), 1);
    assert!(
        parsed
            .attachments()
            .next()
            .expect("recovered part")
            .is_encoding_problem
    );
    assert_leaf_parts(&bytes, policy(1), 1);
    let mut wide = multipart(LEGAL[4].0, LEGAL[4].1, 2);
    wide.truncate(wide.len() - "-- leading space----\r\n".len());
    assert_blocked(
        &wide,
        policy(1),
        "mime_part_limit_exceeded",
        IssueClass::Budget,
    );
}

#[test]
fn mime_recovery_of_missing_boundary_and_header_only_mail_is_unchanged() {
    for content_type in [
        "multipart/mixed; boundary=absent",
        "multipart/mixed",
        "multipart/mixed; boundary=\"\"",
    ] {
        let bytes = format!("{}recovered evidence\r\n", root_headers(content_type)).into_bytes();
        assert_eq!(
            MessageParser::default()
                .parse(&bytes)
                .expect("recoverable fixture")
                .attachment_count(),
            1
        );
        assert_leaf_parts(&bytes, policy(1), 1);
    }
    let bytes = b"From: source@example.test\r\nSubject: headers only\r\n\r\n";
    assert_eq!(
        MessageParser::default()
            .parse(bytes)
            .expect("recoverable fixture")
            .attachment_count(),
        0
    );
    assert_leaf_parts(bytes, policy(1), 0);
}

#[test]
fn mime_truncated_nested_framing_preserves_parser_recovery() {
    let bytes = String::from_utf8(nested_multipart("outer space--", " inner space--"))
        .expect("fixture")
        .replace("-- inner space----\r\n", "")
        .into_bytes();
    assert_eq!(
        MessageParser::default()
            .parse(&bytes)
            .expect("recoverable fixture")
            .attachment_count(),
        2
    );
    assert_leaf_parts(&bytes, policy(3), 2);
}

#[test]
fn mime_raw_nested_message_remains_in_the_same_global_budget() {
    let inner = multipart(LEGAL[4].0, LEGAL[4].1, 2);
    let mut bytes = root_headers("multipart/mixed; boundary=outer");
    write!(bytes, "--outer\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=forwarded.eml\r\n\r\n{}\r\n--outer--\r\n", String::from_utf8(inner).expect("fixture")).expect("fixture");
    let bytes = bytes.into_bytes();
    assert_blocked(
        &bytes,
        policy(1),
        "mime_part_limit_exceeded",
        IssueClass::Budget,
    );
    let bundle = inspect(&bytes, policy(3));
    assert_eq!(bundle.graph.usage.entries, 3);
    assert_eq!(bundle.graph.edges.len(), 3);
    assert_eq!(bundle.graph.objects.len(), 4);
    assert!(
        bundle
            .graph
            .edges
            .iter()
            .all(|edge| edge.outcome == EdgeOutcome::Derived)
    );
    assert!(bundle.graph.issues.is_empty(), "{:#?}", bundle.graph);
}

#[test]
fn mime_digest_keeps_the_current_default_nested_message_semantics() {
    let bytes = format!("{}--digest--\r\nContent-Disposition: attachment; filename=forwarded.eml\r\n\r\nFrom: inner@example.test\r\nSubject: Inner plain message\r\n\r\ninner evidence\r\n--digest----\r\n", root_headers("multipart/digest; boundary=\"digest--\"")).into_bytes();
    let parsed = MessageParser::default()
        .parse(&bytes)
        .expect("digest fixture");
    let part = parsed.attachments().next().expect("nested message");
    assert!(matches!(part.body, PartType::Message(_)));
    assert_leaf_parts(&bytes, policy(1), 1);
}

#[test]
fn mime_width_respects_each_remaining_graph_allowance() {
    let bytes = multipart(LEGAL[3].0, LEGAL[3].1, 2);
    for limits in [
        BudgetLimits {
            max_entries: 1,
            ..BudgetLimits::default()
        },
        BudgetLimits {
            max_edges: 1,
            ..BudgetLimits::default()
        },
        BudgetLimits {
            max_objects: 2,
            ..BudgetLimits::default()
        },
    ] {
        assert_blocked(
            &bytes,
            InspectionPolicy {
                limits,
                ..InspectionPolicy::default()
            },
            "mime_part_limit_exceeded",
            IssueClass::Budget,
        );
    }
}

#[test]
fn mime_metadata_is_global_and_accepts_the_exact_byte_limit() {
    let bytes = multipart(LEGAL[3].0, LEGAL[3].1, 2);
    let metadata =
        (root_headers(LEGAL[3].1).len() + part_headers(0).len() + part_headers(1).len()) as u64;
    let mut tight = policy(2);
    tight.limits.max_container_metadata_bytes = metadata - 1;
    assert_blocked(
        &bytes,
        tight,
        "mime_metadata_limit_exceeded",
        IssueClass::Budget,
    );
    let mut exact = policy(2);
    exact.limits.max_container_metadata_bytes = metadata;
    assert_leaf_parts(&bytes, exact, 2);
}

#[test]
fn mime_unterminated_header_still_has_bounded_metadata_work() {
    let bytes = format!(
        "From: source@example.test\r\nContent-Type: multipart/mixed; boundary=\"{}\r\n\r\n",
        "x".repeat(512)
    )
    .into_bytes();
    let mut tight = policy(1);
    tight.limits.max_container_metadata_bytes = 64;
    assert_blocked(
        &bytes,
        tight,
        "mime_metadata_limit_exceeded",
        IssueClass::Budget,
    );
}

#[test]
fn mime_preflight_preserves_root_parser_and_derived_byte_caps() {
    let bytes = multipart(LEGAL[3].0, LEGAL[3].1, 1);
    let mut root = policy(1);
    root.limits.max_root_bytes = bytes.len() as u64 - 1;
    let error = Inspector::new(root)
        .expect("fixture policy")
        .inspect(ProvidedAttachment::from_bytes(
            "root.eml",
            Some("message/rfc822".to_string()),
            bytes.clone(),
        ))
        .expect_err("existing root limit");
    assert_eq!(error.code, "root_bytes_exceeded");
    let mut parser = policy(1);
    parser.limits.max_parser_input_bytes = bytes.len() as u64 - 1;
    let bundle = inspect(&bytes, parser);
    assert!(
        bundle
            .graph
            .issues
            .iter()
            .any(|issue| issue.code == "parser_input_limit_exceeded")
    );
    assert!(bundle.graph.edges.is_empty());
    let mut derived = policy(1);
    derived.limits.max_total_derived_bytes = bytes.len() as u64 - 1;
    derived.limits.max_object_bytes = derived.limits.max_total_derived_bytes;
    assert_blocked(
        &bytes,
        derived,
        "mime_derived_budget_exceeded",
        IssueClass::Budget,
    );
}

const ENCODED_INNER: &str = concat!(
    "RnJvbTogc291cmNlQGV4YW1wbGUudGVzdA0KU3ViamVjdDogTUlNRSBhZG1pc3Npb24NCk1JTUUtVmVy",
    "c2lvbjogMS4wDQpDb250ZW50LVR5cGU6IG11bHRpcGFydC9taXhlZDsgYm91bmRhcnk9eA0KDQotLXgN",
    "CkNvbnRlbnQtVHlwZTogdGV4dC9wbGFpbg0KQ29udGVudC1EaXNwb3NpdGlvbjogYXR0YWNobWVudDsg",
    "ZmlsZW5hbWU9InBhcnQtMC50eHQiDQoNCnBhcnQgMA0KLS14DQpDb250ZW50LVR5cGU6IHRleHQvcGxh",
    "aW4NCkNvbnRlbnQtRGlzcG9zaXRpb246IGF0dGFjaG1lbnQ7IGZpbGVuYW1lPSJwYXJ0LTEudHh0Ig0K",
    "DQpwYXJ0IDENCi0teC0tDQo=",
);

const ENCODED_GLOBAL_INNER: &str = concat!(
    "RnJvbTogc291cmNlQGV4YW1wbGUudGVzdA0KU3ViamVjdDogTUlNRSBhZG1pc3Npb24gY2Fmw6kNCk1J",
    "TUUtVmVyc2lvbjogMS4wDQpDb250ZW50LVR5cGU6IG11bHRpcGFydC9taXhlZDsgYm91bmRhcnk9eA0K",
    "DQotLXgNCkNvbnRlbnQtVHlwZTogdGV4dC9wbGFpbg0KQ29udGVudC1EaXNwb3NpdGlvbjogYXR0YWNo",
    "bWVudDsgZmlsZW5hbWU9InBhcnQtMC50eHQiDQoNCnBhcnQgMA0KLS14DQpDb250ZW50LVR5cGU6IHRl",
    "eHQvcGxhaW4NCkNvbnRlbnQtRGlzcG9zaXRpb246IGF0dGFjaG1lbnQ7IGZpbGVuYW1lPSJwYXJ0LTEu",
    "dHh0Ig0KDQpwYXJ0IDENCi0teC0tDQo=",
);

fn encoded_inner(content_type: &str) -> Vec<u8> {
    let inner = multipart("x", "multipart/mixed; boundary=x", 2);
    if content_type == "message/global" {
        String::from_utf8(inner)
            .expect("fixture")
            .replacen("Subject: MIME admission", "Subject: MIME admission café", 1)
            .into_bytes()
    } else {
        inner
    }
}

fn encoded_message(content_type: &str, encoding: &str) -> Vec<u8> {
    let inner = encoded_inner(content_type);
    let payload = if encoding == "base64" {
        let encoded = if content_type == "message/global" {
            ENCODED_GLOBAL_INNER
        } else {
            ENCODED_INNER
        };
        let mut output = String::new();
        for (index, chunk) in encoded.as_bytes().chunks(76).enumerate() {
            if index > 0 {
                output.push_str("\r\n");
            }
            output.push_str(std::str::from_utf8(chunk).expect("ASCII fixture"));
        }
        output
    } else {
        let mut output = String::new();
        for (index, byte) in inner.iter().enumerate() {
            if index > 0 && index % 24 == 0 {
                output.push_str("=\r\n");
            }
            write!(output, "={byte:02X}").expect("fixture");
        }
        output
    };
    format!("From: source@example.test\r\nSubject: Encoded nested email\r\nMIME-Version: 1.0\r\nContent-Type: {content_type}\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n{payload}").into_bytes()
}

#[test]
fn mime_locked_parser_eagerly_builds_transfer_encoded_nested_mime() {
    for content_type in ["message/rfc822", "message/global"] {
        for (encoding, expected) in [
            ("base64", Encoding::Base64),
            ("quoted-printable", Encoding::QuotedPrintable),
        ] {
            let bytes = encoded_message(content_type, encoding);
            let parsed = MessageParser::default()
                .parse(&bytes)
                .expect("accepted encoded message");
            let part = parsed.attachments().next().expect("outer nested message");
            assert_eq!(part.encoding, expected);
            let PartType::Message(inner) = &part.body else {
                panic!(
                    "the locked parser did not build the hidden tree: {:#?}",
                    part
                );
            };
            assert_eq!(inner.attachment_count(), 2);
            assert_eq!(inner.parts.len(), 3);
            assert_eq!(inner.raw_message(), encoded_inner(content_type));
            assert!(inner.attachments().all(|part| !part.is_encoding_problem));
        }
    }
}

#[test]
fn mime_encoded_nested_messages_are_honestly_unsupported_before_tree_construction() {
    for content_type in ["message/rfc822", "message/global"] {
        for encoding in ["base64", "quoted-printable"] {
            let bytes = encoded_message(content_type, encoding);
            // RFC 6532 permits transfer-encoded message/global (the fixture
            // contains a UTF-8 header). RFC 2046 excludes these encodings for
            // message/rfc822, but the locked parser accepts them. Both forms
            // formerly yielded useful decoded trees even within graph limits;
            // preserve bytes and report unsupported admission honestly.
            assert_blocked(
                &bytes,
                policy(3),
                "mime_encoded_message_unsupported",
                IssueClass::Unsupported,
            );
        }
    }
    let bytes = format!("{}--digest--\r\nContent-Transfer-Encoding: base64\r\n\r\n{ENCODED_INNER}\r\n--digest----\r\n", root_headers("multipart/digest; boundary=\"digest--\"")).into_bytes();
    let parsed = MessageParser::default()
        .parse(&bytes)
        .expect("encoded digest fixture");
    assert!(matches!(
        parsed.attachments().next().expect("nested message").body,
        PartType::Message(_)
    ));
    assert_blocked(
        &bytes,
        policy(3),
        "mime_encoded_message_unsupported",
        IssueClass::Unsupported,
    );
}

#[test]
fn mime_transfer_encoded_leaf_attachments_remain_supported() {
    for (encoding, payload) in [("base64", "cGFydCAw"), ("quoted-printable", "part=200")] {
        let bytes = format!("{}--simple--\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=leaf.txt\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n{payload}\r\n--simple----\r\n", root_headers(LEGAL[3].1)).into_bytes();
        assert_leaf_parts(&bytes, policy(1), 1);
        let bundle = inspect(&bytes, policy(1));
        let child = bundle.graph.edges[0].child.as_ref().expect("derived child");
        assert_eq!(
            bundle.blobs.get(child).expect("retained leaf").as_ref(),
            b"part 0"
        );
    }
}
