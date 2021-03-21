// Test forced line breaks.

---
// Directly after word.
Line\ Break

// Spaces around.
Line \ Break

// Directly before word does not work.
No \Break

---
// Leading line break.
\ Leading

// Trailing before paragraph break.
Trailing 1 \

Trailing 2

// Trailing before end of document.
Trailing 3 \

---
#let linebreak() = [
    // Inside the old line break definition is still active.
    #circle(radius: 2pt, fill: #000) \
]

A \ B \ C \
