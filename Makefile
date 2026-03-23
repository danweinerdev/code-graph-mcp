BINARY := code-graph-mcp

GOOS := $(shell go env GOOS)
GOARCH := $(shell go env GOARCH)
PLATFORM := $(GOOS)-$(GOARCH)

PLATFORMS := linux-amd64 linux-arm64 darwin-amd64 darwin-arm64 windows-amd64 windows-arm64

.PHONY: build build-all $(PLATFORMS) test test-integration vet clean

build:
	CGO_ENABLED=1 go build -o bin/$(PLATFORM)/$(BINARY) ./cmd/$(BINARY)

build-all: $(PLATFORMS)

linux-amd64:
	CGO_ENABLED=1 GOOS=linux GOARCH=amd64 CC=x86_64-linux-gnu-gcc go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)

linux-arm64:
	CGO_ENABLED=1 GOOS=linux GOARCH=arm64 CC=aarch64-linux-gnu-gcc go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)

darwin-amd64:
	CGO_ENABLED=1 GOOS=darwin GOARCH=amd64 CC=o64-clang go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)

darwin-arm64:
	CGO_ENABLED=1 GOOS=darwin GOARCH=arm64 CC=oa64-clang go build -o bin/$@/$(BINARY) ./cmd/$(BINARY)

windows-amd64:
	CGO_ENABLED=1 GOOS=windows GOARCH=amd64 CC=x86_64-w64-mingw32-gcc go build -o bin/$@/$(BINARY).exe ./cmd/$(BINARY)

windows-arm64:
	CGO_ENABLED=1 GOOS=windows GOARCH=arm64 CC=/opt/llvm-mingw/bin/aarch64-w64-mingw32-gcc go build -o bin/$@/$(BINARY).exe ./cmd/$(BINARY)

test:
	CGO_ENABLED=1 go test -race ./...

test-integration:
	CGO_ENABLED=1 go test -tags integration -race ./internal/tools/ -v

vet:
	go vet ./...

clean:
	rm -rf bin/
