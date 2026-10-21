# Development tasks for travel-ai.

dev:
	cargo run

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

build:
	cargo build --release

web:
	cd src/frontend && mkdir -p dist && cp index.html styles.css app.js dist/

eval VERSION:
	cargo run --release --bin eval -- \
		--config configs/{{VERSION}}.toml \
		--dataset data/eval/queries.jsonl \
		--out results/{{VERSION}}.json \
		--markdown results/{{VERSION}}.md

deploy:
	bash deploy/install.sh

demo:
	curl -N -X POST http://127.0.0.1:8080/api/plan \
		-H 'content-type: application/json' \
		-d '{"query":"Хочу на выходные по Приморью на машине, не больше двух дней"}'

fmt:
	cargo fmt --all

test:
	cargo test
