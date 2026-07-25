# Message System & Serialization

<cite>
**Referenced Files in This Document**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMessages.cpp](file://engine/Poseidon/Network/NetworkMessages.cpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkServerDispatch.cpp](file://engine/Poseidon/Network/NetworkServerDispatch.cpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportMessageQueue.hpp)
- [NetTransportUserMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportUserMessageQueue.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)
- [NetTransportServerSessionQuery.hpp](file://engine/Poseidon/Network/NetTransportServerSessionQuery.hpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)
- [fuzz_decode_msg.cpp](file://apps/fuzzers/Fuzzer/fuzz_decode_msg.cpp)
</cite>

## Table of Contents
1. [Introduction](#introduction)
2. [Project Structure](#project-structure)
3. [Core Components](#core-components)
4. [Architecture Overview](#architecture-overview)
5. [Detailed Component Analysis](#detailed-component-analysis)
6. [Dependency Analysis](#dependency-analysis)
7. [Performance Considerations](#performance-considerations)
8. [Troubleshooting Guide](#troubleshooting-guide)
9. [Conclusion](#conclusion)
10. [Appendices](#appendices)

## Introduction
This document explains the network message system with a focus on message definition, serialization and deserialization, type safety, version compatibility, script value codec, and the message context for request-response patterns and error propagation. It also provides practical guidance for defining custom messages, implementing handlers, debugging serialization issues, optimizing bandwidth, compressing messages, and validating incoming data securely.

## Project Structure
The network message system is implemented under the Poseidon Network module. Key areas include:
- Message definitions and wire format utilities
- Script-to-network value conversion
- Server dispatch and message context
- Transport-level send queues and session management
- Integrity checks and rate limiting

```mermaid
graph TB
subgraph "Network Messages"
A["NetworkMessages.hpp"]
B["NetworkMessages.cpp"]
C["NetworkMsgFormat.hpp"]
end
subgraph "Script Codec"
D["NetworkScriptValueCodec.hpp"]
E["NetworkScriptValueCodec.cpp"]
end
subgraph "Dispatch & Context"
F["NetworkServerDispatch.hpp"]
G["NetworkServerDispatch.cpp"]
H["NetworkMsgContext.cpp"]
end
subgraph "Transport Layer"
I["NetTransportMessageSend.hpp"]
J["NetTransportMessageQueue.hpp"]
K["NetTransportUserMessageQueue.hpp"]
L["NetTransportClientSession.hpp"]
M["NetTransportServerSessionQuery.hpp"]
end
subgraph "Security & Limits"
N["IntegrityCheck.hpp"]
O["RateLimit.hpp"]
end
A --> C
B --> C
D --> E
F --> G
G --> H
I --> J
J --> K
L --> I
M --> L
N --> I
O --> I
```

**Diagram sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMessages.cpp](file://engine/Poseidon/Network/NetworkMessages.cpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkServerDispatch.cpp](file://engine/Poseidon/Network/NetworkServerDispatch.cpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportMessageQueue.hpp)
- [NetTransportUserMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportUserMessageQueue.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)
- [NetTransportServerSessionQuery.hpp](file://engine/Poseidon/Network/NetTransportServerSessionQuery.hpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

**Section sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMessages.cpp](file://engine/Poseidon/Network/NetworkMessages.cpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkServerDispatch.cpp](file://engine/Poseidon/Network/NetworkServerDispatch.cpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportMessageQueue.hpp)
- [NetTransportUserMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportUserMessageQueue.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)
- [NetTransportServerSessionQuery.hpp](file://engine/Poseidon/Network/NetTransportServerSessionQuery.hpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

## Core Components
- Message definitions and registry: central place to declare message types and their fields.
- Wire format utilities: helpers for serializing/deserializing primitives and containers.
- Script value codec: converts between scripting language values and network-safe representations.
- Dispatch and context: routes messages to handlers and manages request-response semantics and errors.
- Transport integration: queues and sessions for sending and receiving messages reliably.
- Security and limits: integrity verification and rate limiting to protect against abuse.

**Section sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMessages.cpp](file://engine/Poseidon/Network/NetworkMessages.cpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkServerDispatch.cpp](file://engine/Poseidon/Network/NetworkServerDispatch.cpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportMessageQueue.hpp)
- [NetTransportUserMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportUserMessageQueue.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)
- [NetTransportServerSessionQuery.hpp](file://engine/Poseidon/Network/NetTransportServerSessionQuery.hpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

## Architecture Overview
The message pipeline spans from application code through the dispatcher to the transport layer. Serialization occurs at the edges (codec and wire format), while dispatch and context manage routing and lifecycle.

```mermaid
sequenceDiagram
participant App as "Application Code"
participant Msg as "Message Registry"
participant Codec as "Script Value Codec"
participant Format as "Wire Format Utils"
participant SendQ as "Send Queue"
participant Session as "Client Session"
participant Net as "Network Stack"
App->>Msg : "Create typed message"
App->>Codec : "Convert script values to network form"
Codec-->>App : "Network-safe payload"
App->>Format : "Serialize payload"
Format-->>App : "Bytes"
App->>SendQ : "Enqueue message"
SendQ->>Session : "Dequeue and send"
Session->>Net : "Transmit bytes"
Note over Session,Net : "Reliable delivery handled by transport"
```

**Diagram sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)

## Detailed Component Analysis

### Message Definitions and Wire Format
- Purpose: Define message IDs, fields, and serialization rules.
- Type safety: Strong typing via message classes and field descriptors ensures correct parsing.
- Version compatibility: Version tags or schema evolution strategies allow forward/backward compatibility.
- Wire format: Compact binary layout with explicit length prefixes and field ordering.

```mermaid
classDiagram
class MessageRegistry {
+register(id, factory)
+resolve(id) Factory
}
class MessageBase {
+id : uint
+version : uint
+serialize(writer)
+deserialize(reader)
}
class WireFormat {
+writeUint(value)
+readUint() uint
+writeBytes(data)
+readBytes(len) Bytes
}
MessageRegistry --> MessageBase : "creates"
MessageBase --> WireFormat : "uses"
```

**Diagram sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMessages.cpp](file://engine/Poseidon/Network/NetworkMessages.cpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)

**Section sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMessages.cpp](file://engine/Poseidon/Network/NetworkMessages.cpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)

### Script Value Codec
- Purpose: Convert scripting language values into network-safe structures and back.
- Supported types: Scalars, arrays, dictionaries, and nested structures.
- Validation: Enforces bounds, allowed enums, and required fields during conversion.
- Error handling: Returns structured errors with context for failed conversions.

```mermaid
flowchart TD
Start(["Start Conversion"]) --> ValidateType["Validate source type"]
ValidateType --> IsScalar{"Is scalar?"}
IsScalar --> |Yes| EncodeScalar["Encode scalar"]
IsScalar --> |No| IsArray{"Is array?"}
IsArray --> |Yes| EncodeArray["Encode array elements"]
IsArray --> |No| IsDict{"Is dictionary?"}
IsDict --> |Yes| EncodeDict["Encode key-value pairs"]
IsDict --> |No| Error["Return conversion error"]
EncodeScalar --> Done(["Done"])
EncodeArray --> Done
EncodeDict --> Done
Error --> Done
```

**Diagram sources**
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)

**Section sources**
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)

### Message Context and Request-Response
- Context object: Carries sender identity, channel, timestamp, and correlation ID.
- Request-response: Correlation IDs link requests to responses; timeouts and retries are managed here.
- Error propagation: Errors are wrapped with context and forwarded to callers.

```mermaid
sequenceDiagram
participant Client as "Client"
participant Srv as "Server Dispatch"
participant Ctx as "Message Context"
participant Resp as "Response Handler"
Client->>Srv : "Request(message, ctx)"
Srv->>Ctx : "Attach correlationId, timestamps"
Srv->>Srv : "Route to handler"
Srv-->>Resp : "Invoke response callback"
Resp-->>Client : "Reply(correlationId, payload)"
Note over Ctx,Srv : "Timeouts and error wrapping applied"
```

**Diagram sources**
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkServerDispatch.cpp](file://engine/Poseidon/Network/NetworkServerDispatch.cpp)

**Section sources**
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkServerDispatch.cpp](file://engine/Poseidon/Network/NetworkServerDispatch.cpp)

### Transport Integration and Queues
- Send queue: Buffers outgoing messages per user/session.
- User message queue: Per-user prioritization and throttling.
- Session: Manages connection state, retransmission, and fragmentation.

```mermaid
classDiagram
class SendQueue {
+enqueue(msg)
+dequeue() msg?
+size() int
}
class UserMessageQueue {
+push(user, msg)
+drain(user) msgs
+clear(user)
}
class ClientSession {
+send(msg)
+flush()
+close()
}
SendQueue <|-- UserMessageQueue : "per-user"
ClientSession --> SendQueue : "uses"
```

**Diagram sources**
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportMessageQueue.hpp)
- [NetTransportUserMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportUserMessageQueue.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)

**Section sources**
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportMessageQueue.hpp)
- [NetTransportUserMessageQueue.hpp](file://engine/Poseidon/Network/NetTransportUserMessageQueue.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)

### Security and Limits
- Integrity check: Validates message authenticity and tamper resistance.
- Rate limit: Controls message throughput per peer to prevent abuse.

```mermaid
flowchart TD
Ingress["Incoming Message"] --> Integrity["Verify Integrity"]
Integrity --> Valid{"Valid?"}
Valid --> |No| Drop["Drop and log"]
Valid --> |Yes| Rate["Apply Rate Limit"]
Rate --> Allowed{"Allowed?"}
Allowed --> |No| Throttle["Throttle/Drop"]
Allowed --> |Yes| Process["Process Message"]
```

**Diagram sources**
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

**Section sources**
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

## Dependency Analysis
High-level dependencies among core components:

```mermaid
graph LR
MsgDef["Message Definitions"] --> WireFmt["Wire Format"]
Codec["Script Codec"] --> WireFmt
Dispatch["Server Dispatch"] --> MsgDef
Dispatch --> Ctx["Message Context"]
SendQ["Send Queue"] --> Session["Client Session"]
Integ["Integrity Check"] --> SendQ
Rate["Rate Limit"] --> SendQ
```

**Diagram sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

**Section sources**
- [NetworkMessages.hpp](file://engine/Poseidon/Network/NetworkMessages.hpp)
- [NetworkMsgFormat.hpp](file://engine/Poseidon/Network/NetworkMsgFormat.hpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkServerDispatch.hpp](file://engine/Poseidon/Network/NetworkServerDispatch.hpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [NetTransportMessageSend.hpp](file://engine/Poseidon/Network/NetTransportMessageSend.hpp)
- [NetTransportClientSession.hpp](file://engine/Poseidon/Network/NetTransportClientSession.hpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

## Performance Considerations
- Minimize allocations: Reuse buffers and avoid temporary copies during serialization.
- Batch messages: Group small updates to reduce overhead.
- Prefer compact types: Use fixed-width integers and enums where possible.
- Avoid deep nesting: Flatten structures when feasible to reduce traversal cost.
- Tune queues: Adjust per-user queue sizes and flush intervals based on latency targets.
- Compression: Enable compression for large payloads only; measure overhead vs benefit.
- Rate limiting: Apply coarse-grained limits first, then fine-grained per-message policies.

[No sources needed since this section provides general guidance]

## Troubleshooting Guide
Common issues and how to diagnose them:
- Serialization failures: Inspect codec error paths and validate input shapes before encoding.
- Deserialization mismatches: Verify message versions and field order; use fuzzing inputs to reproduce.
- Lost responses: Check correlation IDs and timeout settings in the message context.
- Bandwidth spikes: Review batching and compression settings; monitor queue depths.
- Security alerts: Inspect integrity check logs and rate limit triggers.

Use the fuzzer entry point to generate edge-case inputs and validate robustness of decode paths.

**Section sources**
- [fuzz_decode_msg.cpp](file://apps/fuzzers/Fuzzer/fuzz_decode_msg.cpp)
- [NetworkScriptValueCodec.hpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.hpp)
- [NetworkScriptValueCodec.cpp](file://engine/Poseidon/Network/NetworkScriptValueCodec.cpp)
- [NetworkMsgContext.cpp](file://engine/Poseidon/Network/NetworkMsgContext.cpp)
- [IntegrityCheck.hpp](file://engine/Poseidon/Network/IntegrityCheck.hpp)
- [RateLimit.hpp](file://engine/Poseidon/Network/RateLimit.hpp)

## Conclusion
The network message system combines strong typing, clear serialization boundaries, and robust context-driven dispatch to deliver reliable, secure, and efficient communication. By following the guidelines for message design, codec usage, and transport configuration, developers can implement custom messages safely and optimize for performance and security.

[No sources needed since this section summarizes without analyzing specific files]

## Appendices

### Practical Examples

- Defining a custom network message:
  - Create a new message class with an ID and version.
  - Implement serialize/deserialize using wire format utilities.
  - Register the message in the registry.

- Implementing a message handler:
  - Register a handler function for the message ID.
  - Extract parameters from the deserialized payload.
  - Perform validation and business logic.
  - Return a response via the context’s reply mechanism.

- Debugging serialization issues:
  - Log payload sizes and field counts.
  - Compare expected vs actual byte sequences.
  - Use fuzz inputs to identify edge cases.

[No sources needed since this section provides general guidance]