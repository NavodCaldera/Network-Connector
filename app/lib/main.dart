import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;

import 'src/agent_api.dart';
import 'src/home_page.dart';
import 'src/rust/frb_generated.dart';
import 'src/strings.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // Loads nc_bridge (shipped next to the executable; see windows/CMakeLists.txt).
  await RustLib.init(
    externalLibrary:
        Platform.isWindows ? ExternalLibrary.open('nc_bridge.dll') : null,
  );
  runApp(NetworkConnectorApp(api: RustAgentApi()));
}

class NetworkConnectorApp extends StatelessWidget {
  const NetworkConnectorApp({super.key, required this.api});

  final AgentApi api;

  @override
  Widget build(BuildContext context) {
    const seed = Color(0xFF00696B);
    return MaterialApp(
      title: S.appTitle,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: seed),
      ),
      darkTheme: ThemeData(
        colorScheme:
            ColorScheme.fromSeed(seedColor: seed, brightness: Brightness.dark),
      ),
      home: HomePage(api: api),
    );
  }
}
