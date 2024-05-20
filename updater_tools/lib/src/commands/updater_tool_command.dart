import 'package:args/args.dart';
import 'package:args/command_runner.dart';
import 'package:meta/meta.dart';

/// {@template updater_tool_command}
/// A base class for updater tool commands.
/// {@endtemplate}
abstract class UpdaterToolCommand extends Command<int> {
  /// [ArgResults] used for testing purposes only.
  @visibleForTesting
  ArgResults? testArgResults;

  /// [ArgResults] for the current command.
  ArgResults get results => testArgResults ?? argResults!;
}
