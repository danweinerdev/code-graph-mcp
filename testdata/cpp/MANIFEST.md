# Testdata C++ Project — Expected Parse Results

## Expected Symbols

### utils.h
(no definitions — only forward declarations)

### utils.cpp
| Name | Kind | Line | Namespace | Parent |
|------|------|------|-----------|--------|
| clamp | function | 5 | utils | |
| lerp | function | 10 | utils | |
| formatString | function | 14 | utils | |

### engine.h
| Name | Kind | Line | Parent |
|------|------|------|--------|
| Vec2 | struct | 5 | |
| Engine | class | 10 | |
| DebugEngine | class | 22 | |

### engine.cpp
| Name | Kind | Line | Parent |
|------|------|------|--------|
| Engine | method | 4 | Engine |
| update | method | 6 | Engine |
| render | method | 12 | Engine |
| status | method | 16 | Engine |
| dumpState | method | 20 | DebugEngine |

### main.cpp
| Name | Kind | Line |
|------|------|------|
| AppMode | enum | 5 |
| Callback | typedef | 7 |
| main | function | 9 |

### circular_a.h
| Name | Kind | Line |
|------|------|------|
| ClassA | class | 5 |

### circular_b.h
| Name | Kind | Line |
|------|------|------|
| ClassB | class | 5 |

### orphan.cpp
| Name | Kind | Line |
|------|------|------|
| neverCalled | function | 3 |
| alsoOrphaned | function | 7 |

## Expected Call Edges

| From | To | File |
|------|----|------|
| engine.cpp:Engine::update | utils::lerp | engine.cpp |
| engine.cpp:Engine::update | utils::clamp | engine.cpp |
| engine.cpp:Engine::render | utils::formatString | engine.cpp |
| engine.cpp:DebugEngine::dumpState | status | engine.cpp |
| main.cpp:main | update | main.cpp |
| main.cpp:main | render | main.cpp |
| main.cpp:main | utils::clamp | main.cpp |
| main.cpp:main | status | main.cpp |

## Expected Include Edges

| From | To |
|------|----|
| utils.cpp | utils.h |
| utils.h | string |
| engine.h | string |
| engine.cpp | engine.h |
| engine.cpp | utils.h |
| main.cpp | engine.h |
| main.cpp | utils.h |
| main.cpp | iostream |
| circular_a.h | circular_b.h |
| circular_b.h | circular_a.h |
| orphan.cpp | string |

## Expected Inheritance Edges

| Derived | Base |
|---------|------|
| DebugEngine | Engine |

## Key Validation Points

- Forward declarations in utils.h should NOT produce function symbols
- orphan.cpp functions have no incoming call edges
- circular_a.h and circular_b.h include each other (cycle)
- DebugEngine inherits from Engine
- Namespace "utils" is populated for clamp, lerp, formatString
- Method calls (engine.update(), engine.render()) use field_expression pattern
- Qualified calls (utils::clamp) use qualified_identifier pattern
