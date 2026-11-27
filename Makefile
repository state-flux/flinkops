SHELL := /bin/sh
CARGO ?= cargo
PYTHON ?= python3
IMAGE ?= ghcr.io/state-flux/flinkops
TAG ?= 0.1.0
VERSION ?= $(TAG)
# 本机打包格式：Windows 用 zip,setup；Linux 用 tar.gz,deb；macOS 用 tar.gz
FORMATS ?= tar.gz

.PHONY: help build release run test check fmt clippy deny docker helm-lint smoke clean \
        web-dist dist dist-linux dist-src notes release-local

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
	@echo "make web-dist   - 构建前端产物 apps/web/dist"
	@echo "make dist       - 打本机发行包（FORMATS=tar.gz,deb / zip,setup）+ 源码包"
	@echo "make dist-linux - Linux 全套（tar.gz / deb / AppImage，需 squashfs-tools）"
	@echo "make dist-src   - 只打源码包（src.tar.gz / src.zip）"
	@echo "make notes      - 生成 dist/RELEASE_NOTES.md"
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

web-dist:
	cd apps/web && npm ci --no-audit --no-fund && npm run build

# 本机发版：release 二进制 + 前端 + 压缩包/安装包 + 源码包 + SHA256SUMS.txt
# Linux/macOS: FORMATS=tar.gz（Linux 再加 deb）；Windows: FORMATS=zip,setup
dist: release web-dist
	$(PYTHON) scripts/dist.py --version $(VERSION) --formats $(FORMATS),src-tar.gz,src-zip
	@ls -lh dist

# Linux 全套产物：tar.gz + deb + AppImage（需 mksquashfs 与 AppImage runtime，见 docs/release.md）
dist-linux: release web-dist
	$(PYTHON) scripts/dist.py --version $(VERSION) --formats tar.gz,deb,appimage
	@ls -lh dist

# 只打源码快照（git ls-files 清单，不含 target/ dist/ node_modules/）
dist-src:
	$(PYTHON) scripts/dist.py --version $(VERSION) --formats src-tar.gz,src-zip
	@ls -lh dist

notes:
	$(PYTHON) scripts/release_notes.py --version $(VERSION)

# 一条命令产出本机全部发行物 + 发行说明（发布到 GitHub 由 tag 触发 CI）
release-local: dist notes

clean:
	$(CARGO) clean
	rm -rf data
