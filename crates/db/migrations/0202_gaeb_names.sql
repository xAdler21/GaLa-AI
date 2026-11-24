-- LV-Namen in der Preis-Historie: DATAflor-Exportvermerke („GAEB version used: 320“,
-- „DATAflor GAEB-Import“) wurden als Name gelesen. GAEB-Abgaben neu extrahieren, damit der
-- echte LV- bzw. Projektname übernommen wird.
INSERT INTO index_task (document_id, kind, priority)
SELECT DISTINCT p.document_id, 'extract', 5
FROM price_ref p JOIN document d ON d.id = p.document_id
WHERE d.kind = 'gaeb';
