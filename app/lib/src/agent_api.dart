import 'rust/api.dart' as rust;

export 'rust/api.dart' show HelperState, HelperStatus, PolicySummary;

/// The agent calls the UI needs. Mirrors `nc-bridge` 1:1 so widget tests can use a fake.
abstract class AgentApi {
  rust.PolicySummary defaultPolicy();
  Future<void> startHelperSession(rust.PolicySummary policy);
  Future<void> stopHelperSession();
  Stream<rust.HelperStatus> watchStatus();
}

/// The real agent, in-process through flutter_rust_bridge.
class RustAgentApi implements AgentApi {
  @override
  rust.PolicySummary defaultPolicy() => rust.defaultPolicy();

  @override
  Future<void> startHelperSession(rust.PolicySummary policy) =>
      rust.startHelperSession(policy: policy);

  @override
  Future<void> stopHelperSession() => rust.stopHelperSession();

  @override
  Stream<rust.HelperStatus> watchStatus() => rust.watchStatus();
}
