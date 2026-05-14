#pragma once

// Phase 5 synthetic UE fixture — adversarial cases. Each negative case
// asserts that a macro lookalike in a non-code region is NOT stripped.

// (a) Macro lookalike inside a `//` line comment must NOT trigger a strip.
//     Without skip_lexical the scanner would match and rewrite this.
//     UCLASS(InsideLineComment) class FakeFromLineComment {};

/* (b) Macro lookalike inside a block comment must NOT trigger.
   C++ block comments do NOT nest, so we keep the comment body free of
   any embedded `/` `*` sequences — the closing `* /` at the bottom is
   the ONLY comment terminator.
   UCLASS(InsideBlockComment) class FakeFromBlockComment {};
*/

// (c) Real classes — declared AFTER the comments above to confirm the
//     scanner correctly resumed normal scanning past the comment regions.
UCLASS()
class SAMPLE_API URealClassAfterComments : public UObject {
    GENERATED_BODY()
};

// (d) Macro lookalike inside a string literal must NOT trigger a strip.
//     The string is a global const initializer; the parser doesn't
//     extract `kFakeMacroPattern` as a Symbol (it's a variable, not a
//     function/class), but the surrounding class below MUST still
//     extract — proving the string-body skip worked.
const char* const kFakeMacroPattern = "UCLASS(InsideString) class FakeFromString {};";

UCLASS()
class SAMPLE_API URealClassAfterString : public UObject {
    GENERATED_BODY()
};

// (e) Macro with deeply nested parens — exercises find_balanced_close's
//     depth counter.
UCLASS(BlueprintType, meta=(Categories=(One, Two, Three), DisplayName="Deep"))
class SAMPLE_API UDeeplyNestedMeta : public UObject {
    GENERATED_BODY()
};

// (f) Macro with parens INSIDE a string INSIDE a meta block —
//     skip_lexical handles the string; depth counter is unaffected by
//     the string's contents.
UCLASS(meta=(ToolTip="One ) Two ( Three"))
class SAMPLE_API UParenInToolTip : public UObject {
    GENERATED_BODY()
};
