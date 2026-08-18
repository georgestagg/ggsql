import * as assert from 'assert';
import * as vscode from 'vscode';
import { parseCells } from '../cellParser';

function ggsqlDoc(content: string): Thenable<vscode.TextDocument> {
	return vscode.workspace.openTextDocument({ content, language: 'ggsql' });
}

suite('parseCells', () => {
	test('a document with no markers yields no cells', async () => {
		// parseCells only opens a cell at a marker, so a marker-free document has
		// none. Callers rely on this: extension.ts falls back to running the whole
		// document when cells is empty.
		const document = await ggsqlDoc('SELECT 1\nVISUALISE *\n');
		assert.deepStrictEqual(parseCells(document), []);
	});

	test('each marker starts a cell and is excluded from its text', async () => {
		const document = await ggsqlDoc(
			'-- %%\nSELECT 1\n-- %%\nSELECT 2\n-- %%\nSELECT 3\n',
		);
		const cells = parseCells(document);
		assert.strictEqual(cells.length, 3);
		assert.deepStrictEqual(
			cells.map(c => c.text),
			['SELECT 1', 'SELECT 2', 'SELECT 3'],
		);
	});

	test('a marker with no body is dropped', async () => {
		const document = await ggsqlDoc('-- %%\n-- %%\nSELECT 2\n');
		const cells = parseCells(document);
		assert.strictEqual(cells.length, 1);
		assert.strictEqual(cells[0].text, 'SELECT 2');
	});

	test('a cell whose body is only whitespace is dropped', async () => {
		// The marker opens a cell, so this exercises the trim-and-drop path in
		// extractCellText (the whitespace body trims to '' and is filtered out),
		// unlike a document with no marker at all, which never reaches that code.
		const document = await ggsqlDoc('-- %%\n   \n\t\n');
		assert.deepStrictEqual(parseCells(document), []);
	});

	test('the marker is recognised with and without a space', async () => {
		const document = await ggsqlDoc('--%%\nSELECT 1\n--   %%\nSELECT 2\n');
		const cells = parseCells(document);
		assert.strictEqual(cells.length, 2);
		assert.deepStrictEqual(cells.map(c => c.text), ['SELECT 1', 'SELECT 2']);
	});

	test('cell ranges span from the marker to the line before the next', async () => {
		const document = await ggsqlDoc('-- %%\nSELECT 1\nSELECT 2\n-- %%\nSELECT 3\n');
		const cells = parseCells(document);
		assert.strictEqual(cells.length, 2);
		assert.strictEqual(cells[0].range.start.line, 0);
		assert.strictEqual(cells[0].range.end.line, 2);
		assert.strictEqual(cells[1].range.start.line, 3);
		// The document's trailing newline gives it a final empty line (line 5).
		// parseCells closes the last cell at document.lineCount - 1
		// (src/cellParser.ts:59), so the last cell's range extends to that
		// trailing empty line, not to the line holding "SELECT 3" (line 4).
		assert.strictEqual(cells[1].range.end.line, 5);
	});

	test('content before the first marker is ignored', async () => {
		// currentStart is undefined until the first marker, so leading content is
		// not part of any cell.
		const document = await ggsqlDoc('SELECT 0\n-- %%\nSELECT 1\n');
		const cells = parseCells(document);
		assert.strictEqual(cells.length, 1);
		assert.strictEqual(cells[0].text, 'SELECT 1');
	});
});
