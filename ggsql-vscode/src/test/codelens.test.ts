import * as assert from 'assert';
import * as vscode from 'vscode';
import { GgsqlCodeLensProvider } from '../codelens';

function setSqlFiles(value: boolean | undefined): Thenable<void> {
	return vscode.workspace
		.getConfiguration('ggsql')
		.update('enableSqlFiles', value, vscode.ConfigurationTarget.Global);
}

function docOf(content: string, language = 'ggsql'): Thenable<vscode.TextDocument> {
	return vscode.workspace.openTextDocument({ content, language });
}

suite('GgsqlCodeLensProvider', () => {
	let disposables: vscode.Disposable[];
	let provider: GgsqlCodeLensProvider;

	setup(() => {
		disposables = [];
		provider = new GgsqlCodeLensProvider(disposables);
	});

	teardown(async () => {
		disposables.forEach(d => d.dispose());
		await setSqlFiles(undefined);
	});

	test('a single cell gets only Run Query', async () => {
		const document = await docOf('-- %%\nSELECT 1\n');
		const titles = provider.provideCodeLenses(document).map(l => l.command?.title);
		assert.deepStrictEqual(titles, ['$(run) Run Query']);
	});

	test('the first cell has no Run Above and the last no Run Next', async () => {
		const document = await docOf('-- %%\nSELECT 1\n-- %%\nSELECT 2\n-- %%\nSELECT 3\n');
		const lenses = provider.provideCodeLenses(document);

		const byLine = new Map<number, string[]>();
		for (const lens of lenses) {
			const line = lens.range.start.line;
			byLine.set(line, [...(byLine.get(line) ?? []), lens.command!.title]);
		}

		assert.deepStrictEqual(byLine.get(0), ['$(run) Run Query', 'Run Next']);
		assert.deepStrictEqual(byLine.get(2), ['$(run) Run Query', 'Run Above', 'Run Next']);
		assert.deepStrictEqual(byLine.get(4), ['$(run) Run Query', 'Run Above']);
	});

	test('each lens carries its cell start line as the command argument', async () => {
		const document = await docOf('-- %%\nSELECT 1\n-- %%\nSELECT 2\n');
		for (const lens of provider.provideCodeLenses(document)) {
			assert.deepStrictEqual(lens.command?.arguments, [lens.range.start.line]);
		}
	});

	test('sql documents get no lenses while enableSqlFiles is off', async () => {
		const document = await docOf('-- %%\nSELECT 1\n', 'sql');

		await setSqlFiles(true);
		assert.ok(provider.provideCodeLenses(document).length > 0);

		await setSqlFiles(false);
		assert.deepStrictEqual(provider.provideCodeLenses(document), []);
	});
});
