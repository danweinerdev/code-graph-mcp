#pragma once

UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))
class ENGINE_API AActor : public UObject {
    GENERATED_BODY()
public:
    UFUNCTION(BlueprintCallable, Category="Tick")
    virtual void Tick(float DeltaSeconds) override {}
    UPROPERTY(EditAnywhere)
    UAnimMontage* MyMontage;
};
