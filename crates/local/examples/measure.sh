#!/bin/sh
# Builds the CPU and CUDA measurement binaries and measures every curated
# model, then the techniques under evaluation on Qwen3.5 2B. Prints one
# tab-separated table to stdout; progress and notes go to stderr.
#
#   crates/local/examples/measure.sh [TRIALS]
#
# Needs a quiet machine: each run refuses to start above a 1-minute load
# average of MAX_LOAD (default 2.0). Set CUDA_ARCH (default 86) for the GPU
# build, and GPU_LAYERS_<id> style overrides below for models larger than
# the GPU.
set -eu
cd "$(dirname "$0")/../../.."
TRIALS=${1:-200}
MAX_LOAD=${MAX_LOAD:-2.0}
CPU=target/local-native/release/examples/measure
GPU=target/local-cuda/release/examples/measure

RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR=target/local-native \
    cargo build --release -p ghostcaret-local --example measure >&2
if command -v nvcc >/dev/null 2>&1 || [ -x /opt/cuda/bin/nvcc ]; then
    PATH=/opt/cuda/bin:$PATH CUDA_PATH=${CUDA_PATH:-/opt/cuda} CMAKE_CUDA_ARCHITECTURES=${CUDA_ARCH:-86} \
        RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR=target/local-cuda \
        cargo build --release -p ghostcaret-local --features cuda --example measure >&2
else
    GPU=
fi

header=--header
run() {
    bin=$1
    shift
    if [ "$header" = --header ]; then
        "$bin" --trials "$TRIALS" --max-load "$MAX_LOAD" "$@"
        header=
    else
        "$bin" --trials "$TRIALS" --max-load "$MAX_LOAD" --no-header "$@"
    fi
}

# Layers that fit a 4 GB GPU next to a desktop; all layers otherwise.
gpu_layers() {
    case $1 in
        gemma-4-e2b-q4_k_m) echo 20 ;;
        gemma-4-e4b-q4_k_m) echo 12 ;;
        *) echo 4294967295 ;;
    esac
}

for model in qwen3.5-0.8b-base-q4_k_m qwen3.5-2b-base-q4_k_m qwen3.5-4b-base-q4_k_m \
    gemma-4-e2b-q4_k_m gemma-4-e4b-q4_k_m qwen3-1.7b-base-q4_k_m; do
    run "$CPU" --model "$model" --backend cpu
    [ -n "$GPU" ] && run "$GPU" --model "$model" --backend cuda --gpu-layers "$(gpu_layers "$model")"
done

# Techniques: thread count, quantized KV cache, flash attention.
for threads in 4 8 12 16; do
    run "$CPU" --model qwen3.5-2b-base-q4_k_m --backend cpu --threads "$threads"
done
run "$CPU" --model qwen3.5-2b-base-q4_k_m --backend cpu --kv q8_0
run "$CPU" --model qwen3-1.7b-base-q4_k_m --backend cpu --kv q8_0
for fa in on off; do
    run "$CPU" --model qwen3-1.7b-base-q4_k_m --backend cpu --fa "$fa"
    [ -n "$GPU" ] && run "$GPU" --model qwen3-1.7b-base-q4_k_m --backend cuda --fa "$fa"
done

# Spike measurements: n-gram drafting acceptance and batch cost, and the
# size and cost of state checkpoints.
for model in qwen3.5-2b-base-q4_k_m qwen3-1.7b-base-q4_k_m; do
    GHOSTCARET_TEST_MODEL=$model GHOSTCARET_TEST_GPU_LAYERS=0 RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR=target/local-native \
        cargo test --release -p ghostcaret-local --lib spike -- --ignored --nocapture --test-threads 1 2>&1 \
        | grep -E '^[A-Za-z].*(tokens/step|batch cost|partial state)' >&2
done
