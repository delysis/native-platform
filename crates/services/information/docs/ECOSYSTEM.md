# Offline information ecosystem

The kit is format-aware rather than format-imperialist. Its lifecycle model can
represent these families even when a query backend is not yet present:

- Alexandria and compatible SQLite FTS5 corpora for block-cited full text;
- Kiwix/OpenZIM catalogues and ZIM archives for packaged web collections;
- OPDS feeds for publication catalogues;
- STAC catalogues for spatial assets;
- OSM PBF regional extracts and replication-aware snapshots;
- Overture Maps GeoParquet with declared theme and bounding-box selection axes;
- PMTiles for range-addressable vector/raster tiles;
- GeoParquet, FlatGeobuf, GeoJSON sequence, and SQLite/GeoPackage derivatives;
- JSONL/CSV/Parquet knowledge graphs and structured datasets;
- IIIF manifests and OCR/page assets;
- local managed or external read-only resources.

Catalogue support does not imply query support. Each representation advertises
what an installed backend can really do. Current content querying is limited to
the four compiled SQLite profiles. Kiwix ZIM reading and OSM/Overture
materialization or querying are not shipped; those backends can be added later
without changing lifecycle, receipt, tool, or citation contracts.

## OSM, Daylight, and Overture

The newer, broader map corpus relevant here is Overture Maps, not a replacement
name for raw OpenStreetMap. Meta ended new Daylight Map Distribution releases
after version 1.58 and directed users toward Overture, whose releases combine
OSM-derived and other open sources. The subsystem therefore keeps three
different representations:

- raw OSM PBF plus replication state for editable tags, routing, and ODbL
  lineage;
- Overture GeoParquet for globally partitioned, multi-source map features;
- Overture's Global Entity Reference System registry, bridges, and changelog
  for stable cross-release entity references.

Overture's live STAC root discovers releases and assets without hardcoding
`latest`. Core data remains GeoParquet; PMTiles is a visualization artifact, not
the semantic source. Region/theme materialization is still roadmap work, so the
current planner refuses to pretend that a global cloud asset has already become
a deterministic local subset.

Curated catalogue policy should favor redistributable, provenance-rich sources:
Kiwix, Wikimedia dumps, Project Gutenberg, Standard Ebooks, OpenAlex, Crossref,
OpenStreetMap/Geofabrik, Overture, Natural Earth, GeoNames, Open Food Facts,
Internet Archive metadata, and institution-published OPDS/STAC/IIIF feeds.
License and attribution are represented per release rather than guessed from
format or provider.

## Primary specifications and catalogues

- Kiwix public OPDS API: <https://kiwix-tools.readthedocs.io/en/latest/kiwix-serve.html#catalog-v2-opds-api>
- Overture STAC: <https://stac.overturemaps.org/catalog.json>
- Overture data access: <https://docs.overturemaps.org/getting-data/cloud-sources/>
- Overture GERS: <https://docs.overturemaps.org/gers/>
- Daylight transition: <https://daylightmap.org/2024/05/03/sunsetting-daylight.html>
- OSM planet and regional extracts: <https://wiki.openstreetmap.org/wiki/Planet.osm>
- STAC: <https://stacspec.org/en/about/stac-spec/>
- OPDS 2: <https://specs.opds.io/opds-2.0>
- IIIF Presentation 3: <https://iiif.io/api/presentation/3.0/>
- Data Package: <https://datapackage.org/standard/data-package/>
- Croissant: <https://docs.mlcommons.org/croissant/docs/croissant-spec-1.1.html>
- RO-Crate: <https://www.researchobject.org/ro-crate/specification/1.3/index.html>
- BagIt: <https://www.rfc-editor.org/info/rfc8493/>
- TUF metadata: <https://theupdateframework.io/docs/metadata/>
