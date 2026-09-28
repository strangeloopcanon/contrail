# Native history verification

The native-history engine is embedded in both the standalone CLI and the all-in-one package. It uses Python 3 with SQLite and the standard library; no new Rust or Python dependencies are required.

The contract gates are `make check` (workspace formatting and clippy with warnings denied) and `make test` (workspace Rust tests, including the disposable Python history fixtures). Release 0.1.6 CLI / 0.1.5 bundle: `make check`, `make test` and `git diff --check` passed; the test bridge ran 74 Python fixtures. LLM live checks are not applicable to this archive path.

Fixtures cover original-byte roundtrips, committed WAL and continuous writers, closed WAL-header stores, source races, interruption and budget handling, malformed archives, path/link rejection, credential-store projection, immutable incremental chains, narrower overlapping source lists, growing transcripts, cloud configuration, checksum evidence, released-parent continuity and interrupted local release.

Live validation has covered Codex state and thread-history stores, Cursor CLI WAL stores, Cursor desktop workspace/global stores, and Claude Code transcripts. A combined baseline preserved 92,645 native files, including 21,335 SQLite snapshots, in 100 parts totaling 8,310,965,194 compressed bytes. Part/full/member SHA256 and SQLite integrity checks passed twice. A live unchanged Claude follow-up stored zero files, and verified extraction reproduced all 30 baseline files.

Cloud evidence must state its verification level. Correct remote size, parent and permissions alone are size-only verification and cannot authorize local archive release. Provider checksums or authenticated download hashes must cover every handoff file. Upload and scheduling use an external connected Drive workflow, not OAuth credentials in Contrail.

Whole-profile recovery and native app import remain untested. See [coverage and recovery limits](NATIVE-HISTORY.md); managed media, canvases and custom profiles have gaps. Incrementals store complete changed files, not merged message rows. Native deletion remains disabled.
