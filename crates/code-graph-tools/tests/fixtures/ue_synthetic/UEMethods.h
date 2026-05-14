#pragma once

// Phase 5 synthetic UE fixture — UFUNCTION shapes (single-line, multi-arg,
// comma-in-string, comma-in-meta).

UCLASS()
class SAMPLE_API UFunctionExamples : public UObject {
    GENERATED_BODY()
public:
    // (a) UFUNCTION() with no args
    UFUNCTION()
    void EmptyArgs() {}

    // (b) UFUNCTION(BlueprintCallable) — single keyword
    UFUNCTION(BlueprintCallable)
    void BasicCallable() {}

    // (c) UFUNCTION with Category= containing a comma
    //     Tests skip_lexical preventing the comma in "X, Y" from breaking
    //     the balanced-paren walk.
    UFUNCTION(BlueprintCallable, Category="Tick, Game")
    void TickWithCategory() {}

    // (d) UFUNCTION with meta=(DisplayName="A, B") — closing paren inside
    //     a string literal must NOT be counted by find_balanced_close.
    UFUNCTION(BlueprintCallable, meta=(DisplayName="Hello, World"))
    void StringArgWithCommaAndParen() {}

    // (e) Multi-line UFUNCTION args — newlines in the strip range must be
    //     preserved so line numbers below stay aligned.
    UFUNCTION(
        BlueprintCallable,
        Category="Game",
        meta=(DisplayName="Multi-line example")
    )
    void MultilineDeclaration() {}

    // (f) Method after the multi-line block — its line number must be
    //     calculable from the file's source despite the multi-line strip.
    void NormalMethodAfterMultiline() {}
};
