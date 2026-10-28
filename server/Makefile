.PHONY: test build run fmt vet code e2e cross

test:
	go test ./...

build:
	go build -o bin/knock-hub ./cmd/hub

run:
	go run ./cmd/hub -addr :8080

# 打印这台节点的节点码（别人粘这一串就能加你）
code:
	go run ./cmd/hub -data-dir ./data -print-code

fmt:
	go fmt ./...

vet:
	go vet ./...

# 端到端冒烟：起真节点，走开门→查码→announce→关门
e2e:
	cd .. && sh scripts/e2e.sh

# 跨语言：Rust 客户端敲真的 Go 节点
cross:
	cd .. && sh scripts/cross.sh
