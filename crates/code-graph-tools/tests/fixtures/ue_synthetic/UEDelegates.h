#pragma once

// Phase 5 synthetic UE fixture — DECLARE_*_DELEGATE shapes at module scope.
// Delegate macros expand to type definitions; after stripping, only the
// surviving non-macro content (classes below) should produce symbols.

DECLARE_DELEGATE(FSimpleDelegate)

DECLARE_DELEGATE_OneParam(FOneParamDelegate, int)

DECLARE_DELEGATE_TwoParams(FTwoParamDelegate, int, float)

// Multi-line — exercises \n preservation in the fill.
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(
    FMulticastThreeParam,
    int, FirstArg,
    float, SecondArg
)

DECLARE_EVENT(UDelegateHolder, FOnSomethingHappened)

// Class declared AFTER the multi-line delegate macro — its line number must
// be calculable from the source despite the macro spanning multiple lines.
UCLASS()
class SAMPLE_API UDelegateHolder : public UObject {
    GENERATED_BODY()
public:
    void TriggerDelegate() {}
};
