import 'package:file_selector/file_selector.dart' as fs;

/// Native open/save dialogs. Returns paths only; the app never reads job files itself (Rust does).
/// An interface so widget tests can use a fake.
abstract class FilePicker {
  /// Path of a file to open, or null if the owner cancelled.
  Future<String?> openFile({required String label, List<String>? extensions});

  /// Path to save to, or null if the owner cancelled.
  Future<String?> saveFile({required String suggestedName});
}

/// The platform dialogs (package:file_selector).
class SystemFilePicker implements FilePicker {
  const SystemFilePicker();

  @override
  Future<String?> openFile({
    required String label,
    List<String>? extensions,
  }) async {
    final f = await fs.openFile(
      acceptedTypeGroups: [
        if (extensions != null)
          fs.XTypeGroup(label: label, extensions: extensions),
        const fs.XTypeGroup(label: 'All files'),
      ],
    );
    return f?.path;
  }

  @override
  Future<String?> saveFile({required String suggestedName}) async {
    final loc = await fs.getSaveLocation(suggestedName: suggestedName);
    return loc?.path;
  }
}
