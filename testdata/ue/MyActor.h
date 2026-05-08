// Synthetic UE-style header — not actual UE code, hand-crafted for testing.
class UObject {};

class CORE_API AActor : public UObject {
    void Tick(float DeltaTime);
};

class ENGINE_API APawn : public AActor {
    void SetupPlayerInputComponent();
};

class GAMEPLAY_API ACharacter : public APawn {};

class FOO_API BAR_EXTRA UDoubleMacro : public AActor {};

class UNoMacro : public AActor {};
