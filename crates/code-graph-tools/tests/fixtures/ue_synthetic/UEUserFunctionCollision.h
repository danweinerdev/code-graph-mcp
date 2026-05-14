#pragma once

// Phase 5 synthetic UE fixture — documented behavior: when a user lists
// a macro name (e.g. `UCLASS`) and their codebase has a real function
// with that name, the function disappears from the symbol index. This
// is by design (per UeMacroSupport design Decision 4: users should not
// list macro names that collide with real symbols in their code).
//
// This file declares a function named `UCLASS` — with the preset
// enabled, that function MUST NOT extract. The test asserts on its
// absence to pin the "user error → understandable failure" property
// against silent regression.

namespace collision {

// Real function definition named `UCLASS`. Without macro_strip_with_args
// in play, this extracts as a regular Function symbol. WITH the preset,
// the entire definition (signature + body) gets stripped because the
// scanner can't distinguish a real function from a macro use — they
// share an identifier-followed-by-parens shape.
//
// The fixture exists to make this trade-off visible. If a future parser
// change makes user functions named like macros survive stripping, this
// test fails — at which point we'd celebrate and rewrite the assertion.
int UCLASS(int x, int y) {
    return x + y;
}

} // namespace collision

// Real class declared AFTER the collision function — confirms the
// scanner correctly resumed normal scanning past the collision damage.
// This class MUST extract.
UCLASS()
class SAMPLE_API URealClassAfterCollision : public UObject {
    GENERATED_BODY()
};
