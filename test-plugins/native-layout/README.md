# Native CLAP layout interoperability

The [v1 contract](../../docs/clap-device-layout/README.md) defines the extension table and the
bounded document carried by it. These standalone prototypes exercise the contract through real
CLAP plugin entry points.

- [C fixture and host](c-fixture/README.md) exchange the normative document and probe transfer,
  version and extension fallback cases through the public C header.
- [nice-plug prototype](nice-plug/README.md) registers the extension through a patch for the pinned
  wrapper and obtains wire IDs from its canonical CLAP parameter mapping. It checks an independent
  rebuild, reordered parameters and a deliberate hash collision.

## Verify both implementations with the same host

Run from the repository root. Use a CLAP 1.2.8 checkout at
`ca1b43ee2b4f381e94e4170e622d60b92108b128` and a Git repository containing the pinned nice-plug
revision. The combined check is exercised on Linux with a C11 compiler, Python 3 and Cargo.

```sh
fixture=test-plugins/native-layout/c-fixture
contract=docs/clap-device-layout
scratch="$(mktemp -d)"
"$fixture/build.sh" /path/to/clap/include "$contract" "$contract/example.json" "$scratch/c"
test-plugins/native-layout/nice-plug/verify.sh /path/to/nice-plug "$scratch/rust"
python3 -B test-plugins/native-layout/check.py \
    "$scratch/c/probe" "$scratch/c/layout-fixture.clap" "$contract/example.json" \
    --nice-plug-artifacts "$scratch/rust/artifacts"
```

Omit `--nice-plug-artifacts` to run the C transfer and JSON checks alone. The prototype verifier
cross-checks IDs, flags, ranges, slot occupancy, localized labels and schema fallback. Bridge and
application integration use this contract in [#181](https://github.com/klang-studio/komponist/issues/181).
