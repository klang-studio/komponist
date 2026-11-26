import copy
import json
from pathlib import Path
import sys


def encode(layout):
    return json.dumps(layout, ensure_ascii=False, separators=(",", ":")).encode()


def declaration(symbol, data):
    rows = [
        "    " + ", ".join(f"0x{byte:02x}" for byte in data[start : start + 16]) + ","
        for start in range(0, len(data), 16)
    ]
    return "\n".join([f"static const uint8_t {symbol}[] = {{", *rows, "};", ""])


def main(example_path, output_path):
    controls = Path(__file__).with_name("controls.json").read_bytes()
    layout = json.loads(controls)

    unknown_param = copy.deepcopy(layout)
    gain = next(
        cell
        for page in unknown_param["pages"]
        for cell in page["cells"]
        if cell.get("param_id") == 42
    )
    gain["param_id"] = 999

    optional_field = copy.deepcopy(layout)
    optional_field["future_decoration"] = {"style": "example"}
    optional_field["pages"][0]["cells"][0]["future_hint"] = True

    required_capability = copy.deepcopy(layout)
    required_capability["required_capabilities"] = ["future-animation"]

    graphs = Path(example_path).read_bytes()
    invalid_graphs = json.loads(graphs)
    invalid_graphs["pages"][0]["cells"][1]["graph"]["slots"]["attack"] = 999

    output = [
        "/* Generated from the layout examples during the fixture build. */\n",
        declaration("control_layout", controls),
        declaration("unknown_param_layout", encode(unknown_param)),
        declaration("optional_field_layout", encode(optional_field)),
        declaration("required_capability_layout", encode(required_capability)),
        declaration("graph_layout", graphs),
        declaration("invalid_graph_layout", encode(invalid_graphs)),
    ]
    Path(output_path).write_text("\n".join(output), encoding="utf-8")


if __name__ == "__main__":
    main(*sys.argv[1:])
