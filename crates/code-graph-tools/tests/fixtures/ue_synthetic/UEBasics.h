#pragma once

// Synthetic UE fixture — UCLASS shapes and inheritance.
// Each class below exercises a specific parameterized-macro variant.

// (a) Parameterless UCLASS()
UCLASS()
class SAMPLE_API USimpleObject : public UObject {
    GENERATED_BODY()
};

// (b) Single-arg UCLASS(BlueprintType)
UCLASS(BlueprintType)
class SAMPLE_API UBlueprintableObject : public UObject {
    GENERATED_BODY()
};

// (c) Multi-arg with meta=(...) — nested parens stress
UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))
class SAMPLE_API USpawnableObject : public UObject {
    GENERATED_BODY()
};

// (d) Deep meta args — additional nested parens
UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent, Category="UE/Spawnable"))
class SAMPLE_API UAnnotatedObject : public UObject {
    GENERATED_BODY()
};

// (e) Inheritance chain — derived from a UCLASS class
UCLASS()
class SAMPLE_API UDerivedFromBlueprintable : public UBlueprintableObject {
    GENERATED_BODY()
};

// (f) Inheritance chain — two levels deep
UCLASS()
class SAMPLE_API UDeepDerived : public UDerivedFromBlueprintable {
    GENERATED_BODY()
};

// (g) Class with both class-level and method-level reflection macros
UCLASS()
class SAMPLE_API UClassWithMethods : public UObject {
    GENERATED_BODY()
public:
    void PlainMethod() {}
    int GetValue() { return 0; }
};
