# Native Nemotron status

`MAIN.AI_GET_STATUS` now preserves the pinned explicit Nemotron/no-selected-model
branch through the retained Svelte bridge. The renderer may supply an ignored
`customPath`; only the native captured app-data root determines model paths.
Other engines, truthy model selections, downloads and inference remain unsupported.

The bridge takes one structured clone before asynchronous work and validates the
source branch before JSON conversion. This distinguishes JavaScript falsy
selectors from truthy values that JSON would coerce to null, zero or an empty
string. The host also validates the main window, original listener, live full
runtime/generation/path context and explicit bounded native payload before work.
Replies retain the original listener and ordered `{nemotron: {ready, error}}` or
`{nemotron: {ready, localPath}}` shape. Only `{channel, data}` reaches the original
callback; native context and lifetime guards remain private.

One lazy fixed filesystem thread prepares status independently of library IO.
Four whole host leases and four foundation leases include queued work, entered
filesystem calls, ready results and retained native events. An accepted frame
keeps its ownership after its caller stops observing. The real prepared read
survives event clones and native encoding; this does not certify WebView delivery
or disposal of encoded transport buffers. CLOSE seals admission and awaits the
actual retained filesystem-thread join. Four bounded shutdown observers may stop
waiting, time out and retry without losing the join handle. Entered operating
system calls remain nonpreemptible.

The source first checks its revision/file-stat stamp and otherwise hashes five
fixed files in order. A matching stamp deliberately skips hashing, including for
tiny files or directories; `ready` on that branch is source status, not proof of
a usable model. A missing later file takes precedence over an earlier bad hash.
Intel macOS keeps its original unsupported message. A successful hash pass
prepares one opaque stamp intent. Only the existing ordered library owner may
attempt that write, after validating the captured global root and original
request controls. Its observer uses the original cancellation/deadline through
both filesystem preparation and queued library work. Abandoned observers retain
the accepted intent and whole lease until actual disposition. A source-caught
write failure still produces ready status and a private diagnostic.

Ten native host cases cover real missing files/source stamp shortcuts, invalid
admission, caller abort, cloned/encoded ready results, held library work, actual
library selection, supervised filesystem panic, actual stamp success/write
failure, queued cancellation/deadlines, stale ordered intents and timed-out or
abandoned shutdown observers followed by actual join. The stamp cases use a
feature-gated, fixed 75-byte corpus that genuinely traverses the hash path; it
accepts no arbitrary digest/path override. Normal desktop dependencies disable
the feature. Compiler records and byte-checked isolated copies bind each actual
GUI run to its normal binary before test builds can overwrite Cargo outputs.

[Foundation, exact source semantics and bounds](workers/ai-status.md),
[retained desktop source comparisons](workers/ai-status-desktop.md) and
[local integration receipt](baselines/ai-status-imports-linux-local.json).

The scope excludes general engine/provider status, arbitrary model IDs, model
setup/download/deletion, actual large-model verification, execution and speech
recognition. Native foreign-runtime and GUI results require their own CI evidence.
Full feature parity and whole-application performance gains remain unproven.
