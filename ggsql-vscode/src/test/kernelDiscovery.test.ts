import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import {
	discoverKernelPaths,
	generateMetadata,
	isKernelAccessible,
	resolveKernelStrategy,
	selectKernelCandidates,
	type KernelCandidate,
} from '../manager';

const EXTENSION_ID = 'ggsql.ggsql';

// Directories created by the helpers below, removed in suiteTeardown.
const tempDirs: string[] = [];

function tempDir(): string {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ggsql-kernel-'));
	tempDirs.push(dir);
	return dir;
}

const binaryName = process.platform === 'win32' ? 'ggsql-jupyter.exe' : 'ggsql-jupyter';

/**
 * Build an extension directory that looks like an installed platform VSIX,
 * with a stand-in for the kernel at bundled/bin/.
 */
function extensionDirWithBundle(mode = 0o755): { extensionPath: string; kernelPath: string } {
	const extensionPath = tempDir();
	const binDir = path.join(extensionPath, 'bundled', 'bin');
	fs.mkdirSync(binDir, { recursive: true });
	const kernelPath = path.join(binDir, binaryName);
	fs.writeFileSync(kernelPath, '#!/bin/sh\nexit 0\n', { mode });
	fs.chmodSync(kernelPath, mode);
	return { extensionPath, kernelPath };
}

function contextFor(extensionPath: string): vscode.ExtensionContext {
	return { extensionPath } as vscode.ExtensionContext;
}

/**
 * A WorkspaceConfiguration stub covering the two members
 * resolveKernelStrategy() uses. Using a stub rather than writing real settings
 * keeps the migration cases independent of the test instance's config state.
 */
function fakeConfig(values: {
	strategy?: { global?: string; workspace?: string; workspaceFolder?: string };
	kernelPath?: string;
}): vscode.WorkspaceConfiguration {
	return {
		get: (key: string, defaultValue?: unknown) =>
			key === 'kernelPath' ? (values.kernelPath ?? defaultValue) : defaultValue,
		inspect: (key: string) =>
			key === 'kernelStrategy'
				? {
					key: 'ggsql.kernelStrategy',
					defaultValue: 'bundled',
					globalValue: values.strategy?.global,
					workspaceValue: values.strategy?.workspace,
					workspaceFolderValue: values.strategy?.workspaceFolder,
				}
				: undefined,
	} as unknown as vscode.WorkspaceConfiguration;
}

const HOST: KernelCandidate[] = [
	{ kernelPath: '/usr/local/bin/ggsql-jupyter', source: 'System' },
	{ kernelPath: '/opt/homebrew/bin/ggsql-jupyter', source: 'Path' },
];
const hostKernels = () => HOST;
const noHostKernels = () => [];

suite('kernel strategy', () => {
	test('the manifest declares bundled as the default', () => {
		// The rest of the suite assumes this default; it is also what makes the
		// extension work with no kernel installed.
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		const property = extension?.packageJSON.contributes.configuration.properties['ggsql.kernelStrategy'];
		assert.ok(property, 'ggsql.kernelStrategy is not contributed');
		assert.strictEqual(property.default, 'bundled');
		assert.deepStrictEqual(property.enum, ['bundled', 'environment', 'path']);
	});

	test('an unset strategy resolves to bundled', () => {
		assert.strictEqual(resolveKernelStrategy(fakeConfig({})), 'bundled');
	});

	test('a configured kernelPath still means path', () => {
		// Migration: users who set ggsql.kernelPath before the strategy setting
		// existed must keep getting the kernel they named.
		assert.strictEqual(
			resolveKernelStrategy(fakeConfig({ kernelPath: '/opt/ggsql/ggsql-jupyter' })),
			'path',
		);
	});

	test('a whitespace-only kernelPath does not imply path', () => {
		assert.strictEqual(resolveKernelStrategy(fakeConfig({ kernelPath: '   ' })), 'bundled');
	});

	test('an explicit strategy overrides a configured kernelPath', () => {
		// Otherwise a user could never keep a path around while asking for the
		// bundled kernel.
		const config = fakeConfig({
			strategy: { global: 'bundled' },
			kernelPath: '/opt/ggsql/ggsql-jupyter',
		});
		assert.strictEqual(resolveKernelStrategy(config), 'bundled');
	});

	test('workspace scope wins over global scope', () => {
		const config = fakeConfig({ strategy: { global: 'environment', workspace: 'bundled' } });
		assert.strictEqual(resolveKernelStrategy(config), 'bundled');
	});

	test('workspace folder scope wins over workspace scope', () => {
		const config = fakeConfig({ strategy: { workspace: 'bundled', workspaceFolder: 'environment' } });
		assert.strictEqual(resolveKernelStrategy(config), 'environment');
	});

	test('an unknown strategy falls back to bundled', () => {
		// A hand-edited settings.json is not validated before it reaches here.
		const config = fakeConfig({ strategy: { global: 'whatever' } });
		assert.strictEqual(resolveKernelStrategy(config), 'bundled');
	});
});

suite('kernel candidate selection', () => {
	const bundled = '/ext/ggsql.ggsql-0.5.0-darwin-arm64/bundled/bin/ggsql-jupyter';

	test('bundled uses only the bundled kernel', () => {
		const candidates = selectKernelCandidates('bundled', bundled, undefined, hostKernels);
		assert.deepStrictEqual(candidates, [{ kernelPath: bundled, source: 'Bundled' }]);
	});

	test('bundled falls back to host kernels when the build carries none', () => {
		// The platform-neutral VSIX ships no kernel and must keep working the
		// way it does today.
		const candidates = selectKernelCandidates('bundled', undefined, undefined, hostKernels);
		assert.deepStrictEqual(candidates, HOST);
	});

	test('no bundle and no host kernel yields no candidates', () => {
		// Regression test for the phantom runtime: discovery used to append the
		// bare binary name unconditionally, so Positron registered a ggsql
		// runtime that failed at session start with KS-19.
		const candidates = selectKernelCandidates('bundled', undefined, undefined, noHostKernels);
		assert.deepStrictEqual(candidates, []);
	});

	test('environment puts host kernels ahead of the bundled one', () => {
		const candidates = selectKernelCandidates('environment', bundled, undefined, hostKernels);
		assert.deepStrictEqual(candidates, [...HOST, { kernelPath: bundled, source: 'Bundled' }]);
	});

	test('environment falls back to the bundled kernel', () => {
		const candidates = selectKernelCandidates('environment', bundled, undefined, noHostKernels);
		assert.deepStrictEqual(candidates, [{ kernelPath: bundled, source: 'Bundled' }]);
	});

	test('path uses the configured kernel alone', () => {
		// Neither the bundled kernel nor a host install may quietly stand in for
		// the one the user named.
		const candidates = selectKernelCandidates('path', bundled, '/opt/ggsql/ggsql-jupyter', hostKernels);
		assert.deepStrictEqual(candidates, [
			{ kernelPath: '/opt/ggsql/ggsql-jupyter', source: 'Setting' },
		]);
	});

	test('path with no configured kernel behaves as bundled', () => {
		const candidates = selectKernelCandidates('path', bundled, undefined, hostKernels);
		assert.deepStrictEqual(candidates, [{ kernelPath: bundled, source: 'Bundled' }]);
	});
});

suite('kernel accessibility', () => {
	test('a bare binary name is not accessible', async () => {
		// Anything non-absolute reaching this check means the PATH lookup
		// failed; accepting it is the other half of the phantom runtime.
		assert.strictEqual(await isKernelAccessible(binaryName), false);
	});

	test('an executable file is accessible', async () => {
		const { kernelPath } = extensionDirWithBundle();
		assert.strictEqual(await isKernelAccessible(kernelPath), true);
	});

	test('a missing file is not accessible', async () => {
		assert.strictEqual(await isKernelAccessible(path.join(tempDir(), binaryName)), false);
	});

	test('a directory is not accessible', async () => {
		// Directories carry the executable bit on POSIX, so an access() check
		// on its own would pass one.
		assert.strictEqual(await isKernelAccessible(tempDir()), false);
	});
});

suite('bundled kernel discovery', () => {
	test('the bundled kernel is the only candidate under the default strategy', () => {
		const { extensionPath, kernelPath } = extensionDirWithBundle();
		const candidates = discoverKernelPaths(contextFor(extensionPath));
		assert.deepStrictEqual(candidates, [{ kernelPath, source: 'Bundled' }]);
	});

	test('discovery never returns a path that is not on disk', () => {
		// With no bundle, discovery falls through to the host locations. What is
		// installed on the test machine is unknown, but every candidate it
		// reports has to be a real absolute path.
		const candidates = discoverKernelPaths(contextFor(tempDir()));
		for (const candidate of candidates) {
			assert.notStrictEqual(candidate.source, 'Bundled');
			assert.ok(path.isAbsolute(candidate.kernelPath), `${candidate.kernelPath} is not absolute`);
			assert.ok(fs.existsSync(candidate.kernelPath), `${candidate.kernelPath} does not exist`);
		}
	});

	test('a bundled kernel missing its executable bit is repaired', function () {
		// Insurance against an unpack that drops the bit: without the repair the
		// binary would be dropped as inaccessible and no runtime would appear.
		if (process.platform === 'win32') {
			this.skip();
		}
		const { extensionPath, kernelPath } = extensionDirWithBundle(0o644);
		const candidates = discoverKernelPaths(contextFor(extensionPath));
		assert.deepStrictEqual(candidates, [{ kernelPath, source: 'Bundled' }]);
		assert.ok(fs.statSync(kernelPath).mode & 0o111, 'the executable bit was not restored');
	});
});

suite('runtime metadata', () => {
	// generateMetadata reads resources/ggsql-icon.svg from the extension folder,
	// so these use the real one with a stand-in version.
	function realContext(): vscode.ExtensionContext {
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extension, `extension ${EXTENSION_ID} not found`);
		return {
			extensionPath: extension.extensionPath,
			extension: { packageJSON: { version: '9.9.9' } },
		} as unknown as vscode.ExtensionContext;
	}

	test('the bundled runtime id survives an extension update', () => {
		// The bundled kernel lives under the versioned extension directory. An
		// id derived from that path would change on every update, dropping the
		// workspace's runtime affinity and its restorable sessions.
		const context = realContext();
		const before = generateMetadata(context, {
			kernelPath: '/ext/ggsql.ggsql-0.5.0-darwin-arm64/bundled/bin/ggsql-jupyter',
			source: 'Bundled',
		});
		const after = generateMetadata(context, {
			kernelPath: '/ext/ggsql.ggsql-0.6.0-darwin-arm64/bundled/bin/ggsql-jupyter',
			source: 'Bundled',
		});
		assert.strictEqual(before.runtimeId, after.runtimeId);
	});

	test('the bundled runtime is named plain ggsql', () => {
		// It is the default, so there is nothing to distinguish it from.
		const metadata = generateMetadata(realContext(), {
			kernelPath: '/ext/ggsql.ggsql-0.5.0/bundled/bin/ggsql-jupyter',
			source: 'Bundled',
		});
		assert.strictEqual(metadata.runtimeName, 'ggsql');
	});

	test('other runtimes keep a per-path id and a qualified name', () => {
		const context = realContext();
		const system = generateMetadata(context, {
			kernelPath: '/usr/local/bin/ggsql-jupyter',
			source: 'System',
		});
		const setting = generateMetadata(context, {
			kernelPath: '/opt/ggsql/ggsql-jupyter',
			source: 'Setting',
		});
		assert.strictEqual(system.runtimeName, 'ggsql (System)');
		assert.strictEqual(setting.runtimeName, 'ggsql (Setting)');
		assert.notStrictEqual(system.runtimeId, setting.runtimeId);
		assert.notStrictEqual(system.runtimeId, 'ggsql-bundled');
	});
});

suiteTeardown(() => {
	for (const dir of tempDirs) {
		fs.rmSync(dir, { recursive: true, force: true });
	}
});
