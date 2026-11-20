-- Admin-Panel: Durchsatz der letzten Stunde, letzte OCR-Seiten und letzte Fehler,
-- ohne bei jeder Abfrage alle (auch erledigten) Aufgaben zu lesen.
CREATE INDEX index_task_updated_idx ON index_task (updated_at);
CREATE INDEX index_task_error_idx ON index_task (updated_at) WHERE error IS NOT NULL;
