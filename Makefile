SHELL := /bin/sh
CARGO ?= cargo
IMAGE ?= ghcr.io/state-flux/flinkops
TAG ?= 0.1.0

.PHONY: help build release run test check fmt clippy deny docker helm-lint smoke clean

help:
	@echo "make build      - debug 构建"
	@echo "make release    - release 构建（LTO / strip）"
	@echo "make run        - 本地启动（:8080，admin/admin）"
	@echo "make test       - 运行测试"
	@echo "make check      - cargo check 全 workspace"
	@echo "make fmt        - rustfmt"
	@echo "make clippy     - clippy -D warnings"
	@echo "make docker     - 构建镜像"
	@echo "make helm-lint  - helm lint"
	@echo "make smoke      - 启动后打一轮接口"
	@echo "make clean      - 清理构建产物与本地数据"

build:
	$(CARGO) build --workspace

release:
	$(CARGO) build --release -p flinkops-api --bin flinkops
	$(CARGO) build --release -p flinkops-cli
	@ls -lh target/release/flinkops target/release/flinkopsctl 2>/dev/null || true

run:
	FLINKOPS_STORE="sqlite://./data/flinkops.db?mode=rwc" \
	FLINKOPS_CLUSTERS="$${FLINKOPS_CLUSTERS:-local=incluster}" \
	$(CARGO) run -p flinkops-api --bin flinkops

test:
	$(CARGO) test --workspace

check:
	$(CARGO) check --workspace --message-format short

fmt:
	$(CARGO) fmt --all

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

deny:
	$(CARGO) deny check licenses bans sources

docker:
	docker build -f deploy/Dockerfile -t $(IMAGE):$(TAG) .

helm-lint:
	helm lint deploy/helm/flinkops
	helm template flinkops deploy/helm/flinkops >/dev/null && echo "template ok"

smoke:
	@$(CARGO) run -p flinkops-cli -- doctor || true
	@curl -fsS http://127.0.0.1:8080/healthz && echo " healthz ok"
	@curl -fsS -X POST http://127.0.0.1:8080/api/login \
		-H 'content-type: application/json' \
		-d '{"username":"admin","password":"admin"}' >/dev/null && echo "login ok"

clean:
	$(CARGO) clean
	rm -rf data
