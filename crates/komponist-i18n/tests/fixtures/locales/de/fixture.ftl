## Messages for the catalog tests (plurals, variables, terms, literals).

-brand = komponist

# Plain text.
plain = Hallo Welt

# A term and a string literal with braces.
term = { -brand } startet { "{" }jetzt{ "}" }

# Typographic characters stay as they are.
quotes = „Nachtfahrt“ · −3.2 dB

greeting = Hallo { $name }

clips =
    { $n ->
        [0] Keine Clips
        [one] Ein Clip
       *[other] { $n } Clips
    }

takes =
    { $name }: { $n ->
        [one] ein Take
       *[other] { $n } Takes
    }

nested =
    { $a ->
        [one] eins, { $b ->
            [one] x
           *[other] y
        }
       *[other] viele
    }

undo-duplicate = Clips duplizieren
