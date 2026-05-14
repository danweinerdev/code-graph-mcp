#pragma once

// Phase 5 synthetic UE fixture — USTRUCT and UENUM shapes.

USTRUCT(BlueprintType)
struct SAMPLE_API FBasicStruct {
    GENERATED_USTRUCT_BODY()

    UPROPERTY(EditAnywhere)
    int Value;
};

USTRUCT()
struct SAMPLE_API FAnotherStruct {
    GENERATED_USTRUCT_BODY()

    UPROPERTY(BlueprintReadWrite, Category="Data")
    float Scale;

    void HelperMethod() {}
};

UENUM(BlueprintType)
enum class ESimpleEnum : uint8 {
    None     UMETA(DisplayName="None"),
    First    UMETA(DisplayName="First Value"),
    Second   UMETA(DisplayName="Second, Annotated"),
};

UENUM()
enum class EAnotherEnum {
    Alpha,
    Beta,
    Gamma,
};
