#pragma once

class COREUOBJECT_API UObject {
    GENERATED_UCLASS_BODY()
public:
    UFUNCTION(BlueprintCallable)
    virtual void Tick(float DeltaSeconds);
};
