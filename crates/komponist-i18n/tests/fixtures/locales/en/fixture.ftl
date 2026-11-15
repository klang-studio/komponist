## Messages for the catalog tests (plurals, variables, terms, literals).

-brand = komponist

# Plain text.
plain = Hello World

# A term and a string literal with braces.
term = { -brand } starts { "{" }now{ "}" }

# Typographic characters stay as they are.
quotes = “Nachtfahrt” · −3.2 dB

greeting = Hello { $name }

clips =
    { $n ->
        [0] No clips
        [one] One clip
       *[other] { $n } clips
    }

takes =
    { $name }: { $n ->
        [one] one take
       *[other] { $n } takes
    }

nested =
    { $a ->
        [one] one, { $b ->
            [one] x
           *[other] y
        }
       *[other] many
    }

undo-duplicate = Duplicate Clips
