import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;

import 'src/agent_api.dart';
import 'src/app_model.dart';
import 'src/app_shell.dart';
import 'src/rust/api.dart' as rust;
import 'src/rust/frb_generated.dart';
import 'src/strings.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // Loads nc_bridge (shipped next to the executable; see windows/CMakeLists.txt).
  await RustLib.init(
    externalLibrary: Platform.isWindows
        ? ExternalLibrary.open('nc_bridge.dll')
        : null,
  );
  String? dir;
  try {
    dir = rust.settingsDir();
  } catch (_) {
    dir = null; // No app-support folder on this platform yet: settings live in memory only.
  }
  runApp(NetworkConnectorApp(api: RustAgentApi(), settingsDir: dir));
}

class NetworkConnectorApp extends StatefulWidget {
  const NetworkConnectorApp({super.key, required this.api, this.settingsDir});

  final AgentApi api;
  final String? settingsDir;

  @override
  State<NetworkConnectorApp> createState() => _NetworkConnectorAppState();
}

class _NetworkConnectorAppState extends State<NetworkConnectorApp> {
  late final AppModel _model = AppModel(widget.api, widget.settingsDir)
    ..start();

  @override
  void dispose() {
    _model.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    const seed = Color(0xFF00696B);
    return MaterialApp(
      title: S.appTitle,
      theme: ThemeData(colorScheme: ColorScheme.fromSeed(seedColor: seed)),
      darkTheme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: seed,
          brightness: Brightness.dark,
        ),
      ),
      home: AppShell(model: _model),
    );
  }
}
