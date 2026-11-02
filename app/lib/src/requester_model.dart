import 'package:flutter/foundation.dart';

import 'agent_api.dart';
import 'strings.dart';

/// Requester state: jobs recorded on this device and their last known status.
/// Records live in Rust (nc-agent job store), so the list survives an app restart.
class RequesterModel extends ChangeNotifier {
  RequesterModel(this.api, this.dir);

  final AgentApi api;

  /// App folder (null: no folder on this platform, so nothing can be requested).
  final String? dir;

  List<RequestedJob> jobs = const [];

  /// Job ids being checked or fetched right now.
  final busy = <String>{};

  /// Last error per job (plain English).
  final errors = <String, String>{};

  /// Saved-output confirmation per job.
  final saved = <String, String>{};
  bool loaded = false;

  static bool isFinal(JobPhase p) =>
      p == JobPhase.committed || p == JobPhase.disputed || p == JobPhase.failed;

  /// Reads the local records, then asks the coordinator about unfinished jobs.
  Future<void> load() async {
    final d = dir;
    if (d == null) return;
    jobs = await api.listJobs(d);
    loaded = true;
    notifyListeners();
    await refreshAll();
  }

  Future<void> refreshAll() async {
    for (final j in List.of(jobs)) {
      if (!isFinal(j.phase)) await refresh(j.jobId);
    }
  }

  void _put(RequestedJob j) {
    final i = jobs.indexWhere((x) => x.jobId == j.jobId);
    final next = List.of(jobs);
    if (i < 0) {
      next.insert(0, j);
    } else {
      next[i] = j;
    }
    jobs = next;
  }

  Future<void> refresh(String id) async {
    final d = dir;
    if (d == null || busy.contains(id)) return;
    busy.add(id);
    notifyListeners();
    try {
      _put(await api.jobStatus(d, id));
      errors.remove(id);
    } catch (e) {
      errors[id] = e.toString();
    }
    busy.remove(id);
    notifyListeners();
  }

  /// Returns the error or null.
  Future<String?> submit(String module, String input, int replicas) async {
    final d = dir;
    if (d == null) return S.noAppFolder;
    try {
      _put(
        await api.submitJob(
          d,
          SubmitJobRequest(
            modulePath: module,
            inputPath: input,
            replicas: replicas,
          ),
        ),
      );
      notifyListeners();
      return null;
    } catch (e) {
      return e.toString();
    }
  }

  /// Downloads and hash-checks the output into `out`. The result is kept in [saved] or [errors].
  Future<void> fetch(String id, String out) async {
    final d = dir;
    if (d == null || busy.contains(id)) return;
    busy.add(id);
    errors.remove(id);
    saved.remove(id);
    notifyListeners();
    try {
      final f = await api.fetchOutput(d, id, out);
      saved[id] = S.fetched(f.size.toInt(), f.sha256);
    } catch (e) {
      errors[id] = e.toString();
    }
    busy.remove(id);
    notifyListeners();
  }
}
