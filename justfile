# Development tasks for travel-ai.
# Usage: just <recipe>. `just build` and `just dev` stay in debug; the eval
# recipes use release because they report latency numbers.

dev:
	cargo run

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test --all-targets

build:
	cargo build

web:
	npm --prefix src/frontend run build

eval VERSION:
	cargo run --release --bin eval -- \
		--config configs/{{VERSION}}.toml \
		--dataset data/eval/queries.jsonl \
		--out results/{{VERSION}}.json \
		--markdown results/{{VERSION}}.md

eval-all:
	bash scripts/eval_all.sh

docker:
	docker build -f deploy/Dockerfile -t travel-ai:local .

docker-smoke:
	bash scripts/docker_smoke.sh

deploy:
	bash deploy/install.sh

demo:
	curl -N -X POST http://127.0.0.1:8080/api/plan \
		-H 'content-type: application/json' \
		-d '{"query":"Хочу на выходные по Приморью на машине, не больше двух дней"}'

fmt:
	cargo fmt --all

test:
	cargo test --all-targets

frontend-test:
	BASE_URL=http://127.0.0.1:8080 bash scripts/frontend_test.sh

qa:
	bash scripts/qa.sh

edge-cases:
	BASE_URL=http://127.0.0.1:8080 bash scripts/edge_cases.sh
