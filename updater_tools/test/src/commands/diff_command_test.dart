import 'dart:io';

import 'package:args/args.dart';
import 'package:mason_logger/mason_logger.dart';
import 'package:mocktail/mocktail.dart';
import 'package:path/path.dart' as p;
import 'package:scoped_deps/scoped_deps.dart';
import 'package:test/test.dart';
import 'package:updater_tools/src/commands/diff_command.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/packager/patch_packager.dart';

import '../../matchers/matchers.dart';

class _MockArgResults extends Mock implements ArgResults {}

class _MockLogger extends Mock implements Logger {}

class _MockPatchPackager extends Mock implements PatchPackager {}

void main() {
  group(DiffCommand, () {
    late ArgResults argResults;
    late Logger logger;
    late PatchPackager patchPackager;
    late DiffCommand command;

    late File releaseFile;
    late File patchFile;
    late File patchExecutable;
    late File outputFile;

    R runWithOverrides<R>(R Function() body) {
      return runScoped(
        body,
        values: {
          loggerRef.overrideWith(() => logger),
        },
      );
    }

    setUpAll(() {
      registerFallbackValue(Directory(''));
      registerFallbackValue(File(''));
    });

    setUp(() {
      argResults = _MockArgResults();
      logger = _MockLogger();
      patchPackager = _MockPatchPackager();

      final tempDir = Directory.systemTemp.createTempSync();
      releaseFile = File(p.join(tempDir.path, 'release'))
        ..createSync(recursive: true);
      patchFile = File(p.join(tempDir.path, 'patch'))
        ..createSync(recursive: true);
      patchExecutable = File(p.join(tempDir.path, 'patch.exe'))
        ..createSync(recursive: true);
      outputFile = File(p.join(tempDir.path, 'output'));
      when(() => argResults[releaseCliArg]).thenReturn(releaseFile.path);
      when(() => argResults[patchCliArg]).thenReturn(patchFile.path);
      when(() => argResults[patchExecutableCliArg])
          .thenReturn(patchExecutable.path);
      when(() => argResults[outputCliArg]).thenReturn(outputFile.path);

      command = DiffCommand(
        ({required File patchExecutable}) => patchPackager,
      )..testArgResults = argResults;
    });

    test('has a non-empty name', () {
      expect(command.name, isNotEmpty);
    });

    test('has a non-empty description', () {
      expect(command.description, isNotEmpty);
    });

    group('arg validation', () {
      group('when release file does not exist', () {
        setUp(() {
          releaseFile.deleteSync();
        });

        test('logs error and exits with code 64', () async {
          expect(
            await runWithOverrides(command.run),
            equals(ExitCode.usage.code),
          );

          verify(
            () => logger.err(
              any(that: contains('The release file does not exist')),
            ),
          );
        });
      });

      group('when patch file does not exist', () {
        setUp(() {
          patchFile.deleteSync();
        });

        test('logs error and exits with code 64', () async {
          expect(
            await runWithOverrides(command.run),
            equals(ExitCode.usage.code),
          );

          verify(
            () => logger.err(
              any(that: contains('The patch file does not exist')),
            ),
          );
        });
      });

      group('when patch executable does not exist', () {
        setUp(() {
          patchExecutable.deleteSync();
        });

        test('logs error and exits with code 64', () async {
          expect(
            await runWithOverrides(command.run),
            equals(ExitCode.usage.code),
          );

          verify(
            () => logger.err(
              any(that: contains('The patch-executable file does not exist')),
            ),
          );
        });
      });
    });

    group('when args are valid', () {
      setUp(() {
        when(
          () => patchPackager.makeDiff(
            base: any(named: 'base'),
            patch: any(named: 'patch'),
            outFile: any(named: 'outFile'),
          ),
        ).thenAnswer((_) async {});
      });

      test('forwards values to patchPackager', () {
        expect(
          runWithOverrides(command.run),
          completion(ExitCode.success.code),
        );

        verify(
          () => patchPackager.makeDiff(
            base: any(
              named: 'base',
              that: equalsFileSystemEntity(releaseFile),
            ),
            patch: any(
              named: 'patch',
              that: equalsFileSystemEntity(patchFile),
            ),
            outFile: any(
              named: 'outFile',
              that: equalsFileSystemEntity(outputFile),
            ),
          ),
        ).called(1);
      });
    });
  });
}
