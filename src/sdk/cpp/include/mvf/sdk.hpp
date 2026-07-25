// MVF C++ module SDK — control plane over stdio, data plane over shared memory.
//
// A payload is byte-based and typed: it lives in the shared-memory arena as
// [descriptor header | payload bytes] and reaches the module as a zero-copy
// Payload (a pointer into the mapped arena you may also view as a typed buffer).
// The type is self-describing — media type, dtype and shape come from the header —
// so a module author knows exactly what an edge carries. There is no base64;
// payloads never travel inline.
//
// A classifier is a function `Classification classify(const Payload&, const json& meta)`.
// A processor is a function `std::optional<Output> transform(const Payload&, const json& meta)`
// returning a new frame, or std::nullopt to drop it. The SDK owns the stdio loop and the
// descriptor decoding. See ../../../../protocol/README.md. Local only — no network.
//
// This SDK mirrors the reference Python SDK (src/sdk/python/mvf_sdk/__init__.py) and speaks
// the exact same wire protocol, so a C++ module is interchangeable with a Python or .NET one.

#ifndef MVF_SDK_HPP
#define MVF_SDK_HPP

#include <cstdint>
#include <cstddef>
#include <functional>
#include <optional>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

namespace mvf {

using json = nlohmann::json;

// ---- typed-payload descriptor constants (must match Mvf.Abstractions.PayloadDescriptor) ----
constexpr uint32_t kMagic = 0x3146564Du; // 'MVF1' little-endian
constexpr uint16_t kVersion = 1;
constexpr size_t kHeaderSize = 192;
constexpr int kMaxRank = 8;

enum class MediaType : uint16_t {
    Blob = 0,
    Tensor = 1,
    Image = 2,
    Json = 3,
};

enum class ElementType : uint8_t {
    UInt8 = 0,
    Int8 = 1,
    UInt16 = 2,
    Int16 = 3,
    UInt32 = 4,
    Int32 = 5,
    UInt64 = 6,
    Int64 = 7,
    Float16 = 8,
    BFloat16 = 9,
    Float32 = 10,
    Float64 = 11,
};

// Byte size of one element of the given type.
size_t element_size(ElementType type);

// A typed, byte-based payload read in place from the arena (zero copy).
// `data`/`size` point directly into the mapped arena slot and are valid only
// for the duration of the callback that received the Payload.
struct Payload {
    MediaType media_type = MediaType::Blob;
    ElementType element_type = ElementType::UInt8;
    std::vector<int64_t> shape;
    const uint8_t* data = nullptr;
    size_t size = 0;

    const uint8_t* begin() const { return data; }
    const uint8_t* end() const { return data + size; }
};

// A typed payload a processor emits. Use blob() or tensor() to build one.
struct Output {
    std::string data; // raw payload bytes
    MediaType media_type = MediaType::Blob;
    ElementType element_type = ElementType::UInt8;
    std::vector<int64_t> shape;
};

// A raw byte blob (u8).
Output blob(std::string bytes);
Output blob(const void* data, size_t size);

// A typed tensor: raw `bytes` with an `element_type` and `shape`.
Output tensor(std::string bytes, ElementType element_type, std::vector<int64_t> shape,
              MediaType media_type = MediaType::Tensor);

// The result of a classifier: a label plus optional measurement/unit/details.
struct Classification {
    std::string label;
    std::optional<double> measurement;
    std::optional<std::string> unit;
    std::optional<std::string> details;
};

// ---- module callbacks ----
using ClassifyFn = std::function<Classification(const Payload&, const json& meta)>;
using TransformFn = std::function<std::optional<Output>(const Payload&, const json& meta)>;

// Optional lifecycle hooks. Provide default-constructed (empty) std::function to skip.
struct ModuleHooks {
    // Warmup after the hello handshake (load a model, connect a device). When set, the
    // module signals readiness only after this returns (sd_notify READY=1 style).
    std::function<void()> on_start;
    // Capture durable state at a cycle boundary. Return std::nullopt when stateless.
    std::function<std::optional<Output>()> on_checkpoint;
    // Rehydrate state captured earlier (usually after a restart).
    std::function<void(const Payload&)> on_restore;
};

// Emit a diagnostic log line to the engine (protocol {"type":"log"}). Appears in the CLI dashboard
// (the node's log panel) and, headless, on the engine's stderr — the way a module reports what it is
// doing without polluting the typed data plane. Safe to call from classify/transform/on_start: the
// stdio loop is single-threaded, so the line is ordered and never collides with a result.
// level is "debug"/"info"/"warn"/"error".
void log(const std::string& message, const std::string& level = "info");

// Run the stdio loop for a classifier module. Blocks until the engine shuts the module down.
int run_classifier(const std::string& module_id, ClassifyFn classify, ModuleHooks hooks = {});

// Run the stdio loop for a processor (transformer) module: frame in -> new frame out.
int run_processor(const std::string& module_id, TransformFn transform, ModuleHooks hooks = {});

} // namespace mvf

#endif // MVF_SDK_HPP
