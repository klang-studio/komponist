"""Semantic checks for the native-layout interoperability prototype."""

import json
import math
import re

MAX_BYTES = 65_536
INVALID_CLAP_ID = 0xFFFF_FFFF
STEPPED = 1 << 0
HIDDEN = 1 << 2
READ_ONLY = 1 << 3
BYPASS = 1 << 4
ENUM = 1 << 16

LOCALE = re.compile(r"[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8}){0,2}")
PAGE_KEY = re.compile(r"[a-z][a-z0-9_-]{0,31}")
CAPABILITY = re.compile(r"[a-z][a-z0-9._-]{0,31}")

# Min span, max span, and whether the control occupies a half row.
WIDGETS = {
    "knob": (1, 1, False),
    "knob_large": (2, 2, False),
    "number": (1, 1, True),
    "toggle_button": (1, 12, True),
    "segment": (1, 12, True),
    "dropdown": (2, 2, True),
}


class InvalidLayout(ValueError):
    """The plugin supplied a malformed layout."""


class UnsupportedLayout(ValueError):
    """The host should use the standard card for this layout."""


def require(condition, message):
    if not condition:
        raise InvalidLayout(message)


def unique_object(items):
    result = {}
    for key, value in items:
        require(key not in result, f"duplicate object key {key!r}")
        result[key] = value
    return result


def invalid_constant(token):
    raise InvalidLayout(f"invalid JSON constant {token}")


def read_json(data):
    return json.loads(
        data.decode("utf-8"),
        object_pairs_hook=unique_object,
        parse_constant=invalid_constant,
    )


def integer(value, low, high, description):
    require(type(value) is int and low <= value <= high, description)
    return value


def sequence(value, low, high, description):
    require(isinstance(value, list) and low <= len(value) <= high, description)
    return value


def label_map(labels, locales, required_locale=None):
    require(isinstance(labels, dict) and bool(labels), "invalid labels")
    seen = set()
    for locale, label in labels.items():
        require(LOCALE.fullmatch(locale), "invalid label locale")
        key = locale.lower()
        require(key in locales and key not in seen, "invalid label locale")
        seen.add(key)
        require(isinstance(label, str) and bool(label.strip()), "empty label")
        try:
            size = len(label.encode("utf-8"))
        except UnicodeEncodeError as error:
            raise InvalidLayout("invalid Unicode in label") from error
        require(size <= 128, "label exceeds 128 UTF-8 bytes")
        require(not any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in label),
                "control character in label")
    require(required_locale is None or required_locale in seen, "missing default label")


def parameter_inventory(params):
    result = {}
    for param in params:
        param_id = integer(param["id"], 0, INVALID_CLAP_ID - 1, "invalid inventory ID")
        require(param_id not in result, "ambiguous inventory ID")
        result[param_id] = param
    return result


def check_choices(cell, flags, low, high, locales, default_locale):
    widget = cell["widget"]
    if widget == "toggle_button":
        require(flags & STEPPED and low == 0 and high == 1, "toggle needs stepped range 0..1")
    if widget not in {"segment", "dropdown"}:
        require("choices" not in cell, "choices on a non-choice widget")
        return
    require(flags & STEPPED and flags & ENUM, "choice widget needs a stepped enum")
    choices = sequence(cell.get("choices"), 2, 16, "invalid number of choices")
    require(-2**31 <= low < high <= 2**31 - 1 and high - low + 1 == len(choices),
            "choice range must match CLAP")
    for offset, choice in enumerate(choices):
        require(isinstance(choice, dict), "invalid choice")
        value = integer(choice.get("value"), -2**31, 2**31 - 1, "non-integral choice")
        require(value == low + offset, "choices must cover the CLAP range in order")
        label_map(choice.get("labels"), locales, default_locale)


def check_cell(cell, columns, params, locales, default_locale):
    require(isinstance(cell, dict), "invalid cell")
    widget = cell.get("widget")
    require(isinstance(widget, str) and widget in WIDGETS, "unknown widget")
    min_span, max_span, split = WIDGETS[widget]
    col = integer(cell.get("col"), 0, columns - 1, "column outside page")
    row = integer(cell.get("row"), 0, 2, "row outside page")
    cols = integer(cell.get("cols"), min_span, max_span, "invalid column span")
    integer(cell.get("rows"), 1, 1, "invalid row span")
    require(col + cols <= columns, "column span outside page")
    if split:
        half = cell.get("half")
        require(half in ("top", "bottom"), "half-row control needs a lane")
        lanes = (half,)
    else:
        require("half" not in cell, "full-row control cannot specify a lane")
        lanes = ("top", "bottom")
    param_id = integer(cell.get("param_id"), 0, INVALID_CLAP_ID - 1, "invalid parameter ID")
    require(param_id in params, f"unknown CLAP parameter ID {param_id}")
    param = params[param_id]
    flags = param["flags"]
    low, high = param["min"], param["max"]
    require(not flags & (HIDDEN | READ_ONLY | BYPASS), "unsupported parameter flags")
    require(math.isfinite(low) and math.isfinite(high) and low < high, "invalid CLAP range")
    if "labels" in cell:
        label_map(cell["labels"], locales)
    check_choices(cell, flags, low, high, locales, default_locale)
    return {(row, column, lane) for column in range(col, col + cols) for lane in lanes}


def validate(layout, params, size):
    """Check a decoded fixed-control layout descriptor against the plugin's CLAP inventory."""
    integer(size, 1, MAX_BYTES, "layout size outside limit")
    require(isinstance(layout, dict), "layout must be an object")
    capabilities = sequence(layout.get("required_capabilities", []), 0, 8, "invalid capabilities")
    require(all(isinstance(cap, str) and CAPABILITY.fullmatch(cap) for cap in capabilities),
            "invalid capability name")
    require(len(set(capabilities)) == len(capabilities), "duplicate capability")
    if capabilities:
        raise UnsupportedLayout("capabilities outside the fixed-control fixture")

    tags = sequence(layout.get("locales"), 1, 8, "invalid locale count")
    require(all(isinstance(tag, str) and LOCALE.fullmatch(tag) for tag in tags),
            "invalid locale tag")
    locales = {tag.lower() for tag in tags}
    require(len(locales) == len(tags), "duplicate locale")
    default = layout.get("default_locale")
    require(isinstance(default, str) and LOCALE.fullmatch(default) and default.lower() in locales,
            "invalid or undeclared default locale")
    default = default.lower()
    inventory = parameter_inventory(params)
    page_keys = set()
    for page in sequence(layout.get("pages"), 1, 4, "invalid page count"):
        require(isinstance(page, dict), "invalid page")
        key = page.get("key")
        require(isinstance(key, str) and PAGE_KEY.fullmatch(key), "invalid page key")
        require(key not in page_keys, "duplicate page key")
        page_keys.add(key)
        label_map(page.get("labels"), locales, default)
        columns = integer(page.get("columns"), 1, 14, "invalid column count")
        occupied = set()
        for cell in sequence(page.get("cells"), 1, 84, "invalid cell count"):
            slots = check_cell(cell, columns, inventory, locales, default)
            require(occupied.isdisjoint(slots), "overlapping cells")
            occupied.update(slots)
