# C CLAP layout interoperability fixture

This fixture uses the public C extension header and the upstream CLAP headers. It builds a CLAP
module that exposes the normative layout example and a separate C host that loads the module,
creates a plugin instance, reads the standard CLAP parameter inventory, and fetches its layout.

Use the upstream [CLAP 1.2.8 headers](https://github.com/free-audio/clap/tree/ca1b43ee2b4f381e94e4170e622d60b92108b128/include),
a C11 compiler and Python 3. From the repository root:

```sh
fixture=test-plugins/native-layout/c-fixture
contract=docs/clap-device-layout
output="$(mktemp -d)"
"$fixture/build.sh" /path/to/clap/include "$contract" "$contract/example.json" "$output"
"$output/probe" --self-test "$output/layout-fixture.clap"
"$output/probe" "$output/layout-fixture.clap" org.klang-studio.komponist.layout-fixture.valid
```

On Windows, run the build with Git Bash and a MinGW compiler. The probe is named `probe.exe`.
The fixture module is a single shared library on each platform and the probe loads that file
directly.

The probe reports CLAP parameter IDs, flags, names, and ranges alongside the layout and transfer
status as JSON. Successful transfers preserve the raw UTF-8 JSON bytes. The probe emits `null`
for absent or inconsistent transfers. It leaves schema and parameter-link validation to the caller.

The factory publishes twelve variants under the prefix `org.klang-studio.komponist.layout-fixture.`.

| Suffix | Response |
| --- | --- |
| `valid` | The normative layout document |
| `missing` | No layout extension |
| `absent` | v1 extension reports `ABSENT` |
| `incompatible` | Only a different extension ID |
| `invalid-length` | Zero byte count in the length query |
| `wrong-copy-length` | Byte count changes between the query and copy |
| `oversize` | Length query above the maximum |
| `unknown-param` | A cell refers to parameter ID 999 |
| `optional-field` | Unknown optional metadata on the root and a cell |
| `required-capability` | Unknown required capability |
| `version-two` | Declarative graph panel and an authored canvas |
| `invalid-version-two` | A v2 graph refers to unknown parameter ID 999. The v1 panel remains available |
