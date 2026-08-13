#!/bin/sh
# Test-only bounded stdio fixture. It never satisfies native model readiness.
body='{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Returns the supplied value","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}}],"content":[{"type":"text","text":"fixture tool result"}]}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
