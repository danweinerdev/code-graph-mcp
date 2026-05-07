# Intentionally malformed: tree-sitter parses this with ERROR nodes.
# The parser must skip error nodes gracefully without panic.

def foo(:
    pass

def good():
    pass
