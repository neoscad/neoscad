// The language client's wiring (src/language.js) against a stand-in
// server: the handshake over the bridge's transport, and the capabilities
// the editor asks for. The features themselves need a DOM and the real
// server; the app's tests drive those (apple/AppTests/LanguageTests.swift).

import assert from "node:assert/strict";
import { test } from "node:test";
import { lspTransport } from "../src/bridge.js";
import { languageClient } from "../src/language.js";

test("the client initialises over the bridge and asks for what the editor uses", async () => {
  const sent = [];
  const transport = lspTransport((m) => sent.push(JSON.parse(m.message)));
  const client = languageClient(transport, () => {});
  assert.equal(sent.length, 1);
  const init = sent[0];
  assert.equal(init.method, "initialize");
  const caps = init.params.capabilities.textDocument;
  // Versioned diagnostics (the server tags each publish with the version
  // it evaluated), snippets for builtin completions, markdown docs, and
  // signature parameters as offsets into the label.
  assert.equal(caps.publishDiagnostics.versionSupport, true);
  assert.equal(caps.completion.completionItem.snippetSupport, true);
  assert.ok(caps.hover.contentFormat.includes("markdown"));
  assert.equal(caps.signatureHelp.signatureInformation.parameterInformation.labelOffsetSupport, true);
  transport.receive(
    JSON.stringify({
      jsonrpc: "2.0",
      id: init.id,
      result: { capabilities: { positionEncoding: "utf-16", textDocumentSync: { openClose: true, change: 2 }, hoverProvider: true } },
    }),
  );
  await client.initializing;
  assert.equal(client.serverCapabilities.hoverProvider, true);
  assert.equal(sent[1].method, "initialized");
});
