BINARY := code-graph-mcp

GOOS := $(shell go env GOOS)
GOARCH := $(shell go env GOARCH)
PLATFORM := $(GOOS)-$(GOARCH)

PLATFORMS := linux-amd64 linux-arm64 darwin-amd64 darwin-arm64 windows-amd64 windows-arm64

.PHONY: build build-all $(PLATFORMS) test test-integration vet clean

build: $(PLATFORM)

build-all: $(PLATFORMS)

linux-amd64:
ifeq ($(GOOS)-$(GOARCH),linux-amd64)
	CGO_ENABLED=1 go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)
else
	@command -v x86_64-linux-gnu-gcc >/dev/null 2>&1 || { echo "Skipping $@ (x86_64-linux-gnu-gcc not found)"; exit 0; }
	CGO_ENABLED=1 GOOS=linux GOARCH=amd64 CC=x86_64-linux-gnu-gcc go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)
endif

linux-arm64:
ifeq ($(GOOS)-$(GOARCH),linux-arm64)
	CGO_ENABLED=1 go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)
else
	@command -v aarch64-linux-gnu-gcc >/dev/null 2>&1 && \
		CGO_ENABLED=1 GOOS=linux GOARCH=arm64 CC=aarch64-linux-gnu-gcc go build -o bin/$@/$(BINARY) ./cmd/$(BINARY) || \
		echo "Skipping $@ (aarch64-linux-gnu-gcc not found)"
endif

darwin-amd64:
ifeq ($(GOOS)-$(GOARCH),darwin-amd64)
	CGO_ENABLED=1 go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)
else
	@command -v o64-clang >/dev/null 2>&1 && \
		CGO_ENABLED=1 GOOS=darwin GOARCH=amd64 CC=o64-clang go build -o bin/$@/$(BINARY) ./cmd/$(BINARY) || \
		echo "Skipping $@ (o64-clang not found)"
endif

darwin-arm64:
ifeq ($(GOOS)-$(GOARCH),darwin-arm64)
	CGO_ENABLED=1 go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)
else
	@command -v oa64-clang >/dev/null 2>&1 && \
		CGO_ENABLED=1 GOOS=darwin GOARCH=arm64 CC=oa64-clang go build -o bin/$@/$(BINARY) ./cmd/$(BINARY) || \
		echo "Skipping $@ (oa64-clang not found)"
endif

windows-amd64:
ifeq ($(GOOS)-$(GOARCH),windows-amd64)
	CGO_ENABLED=1 go build -o bin/$@/$(BINARY).exe ./cmd/$(BINARY)
else
	@command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 && \
		CGO_ENABLED=1 GOOS=windows GOARCH=amd64 CC=x86_64-w64-mingw32-gcc go build -o bin/$@/$(BINARY).exe ./cmd/$(BINARY) || \
		echo "Skipping $@ (x86_64-w64-mingw32-gcc not found)"
endif

windows-arm64:
ifeq ($(GOOS)-$(GOARCH),windows-arm64)
	CGO_ENABLED=1 go build -o bin/$@/$(BINARY).exe ./cmd/$(BINARY)
else
	@command -v /opt/llvm-mingw/bin/aarch64-w64-mingw32-gcc >/dev/null 2>&1 && \
		CGO_ENABLED=1 GOOS=windows GOARCH=arm64 CC=/opt/llvm-mingw/bin/aarch64-w64-mingw32-gcc go build -o bin/$@/$(BINARY).exe ./cmd/$(BINARY) || \
		echo "Skipping $@ (aarch64-w64-mingw32-gcc not found)"
endif

test:
	CGO_ENABLED=1 go test -race ./...

test-integration:
	CGO_ENABLED=1 go test -tags integration -race ./internal/tools/ -v

vet:
	go vet ./...

clean:
	rm -rf bin/
