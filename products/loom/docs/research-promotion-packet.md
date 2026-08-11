# Research promotion packet v1

Loom can import a reviewed mixed-authorship result from **Writing options →
Import reviewed research…**. The native host, not the renderer, opens the file
picker and reads the selected packet. The packet is evidence to validate; it is
not promotion authority.

The selected file must be a regular, non-symlink JSON file no larger than
262,144 bytes. Its exact shape is:

```json
{
  "schema": "loom.research-promotion-packet.v1",
  "document_id": "<active Loom document ULID>",
  "record": {
    "id": "<mixed-authorship assembly ULID>",
    "output_blob_id": "<SHA-256 of result_text UTF-8 bytes>",
    "output_byte_len": 123,
    "operation_graph": {
      "nodes": ["<bounded PipelineOperation JSON values>"],
      "output": "<output operation ULID>"
    }
  },
  "result_text": "<exact reviewed manuscript result>"
}
```

Import deserialization revalidates the bounded operation graph. The live
project store then verifies the exact result bytes, records a new
`MixedAuthorshipAdmission`, derives the current visible source revision and a
fresh command occurrence itself, and retains the resulting lease only in
process memory. Persisted packet or admission rows cannot confirm a promotion.

After import, Loom shows the exact result in its explicit research-selection
dialog. Confirmation still requires a fresh native focus sample and consumes a
one-use challenge bound to the application session, window, document, result,
and command fingerprint. Cancelling the file picker stages nothing.
