#pragma once

// Synthetic UE fixture — UPROPERTY shapes. Fields are not extracted
// as Symbol records by the parser (fields aren't in the definition queries),
// so this file's role is to confirm UPROPERTY(...) macros don't break the
// surrounding class extraction even when their args are complex.

UCLASS()
class SAMPLE_API UPropertyExamples : public UObject {
    GENERATED_BODY()
public:
    UPROPERTY(EditAnywhere)
    int SimpleProperty;

    UPROPERTY(BlueprintReadWrite, Category="Game")
    float CategorizedProperty;

    UPROPERTY(EditAnywhere, BlueprintReadWrite, meta=(DisplayName="Health, Mana"))
    int StringArgWithComma;

    UPROPERTY(
        EditAnywhere,
        BlueprintReadWrite,
        meta=(ClampMin="0", ClampMax="100", DisplayName="Multi-line")
    )
    float MultilineDeclaration;

    // Method after multiple properties — confirms class body still parses.
    void RegularMethod() {}
};

UCLASS()
class SAMPLE_API UAnotherPropertyHolder : public UObject {
    GENERATED_BODY()
public:
    UPROPERTY(VisibleAnywhere)
    int Counter;

    void AccessorMethod() {}
};
