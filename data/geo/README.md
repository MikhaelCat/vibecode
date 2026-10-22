# Geo data

- `primorye_krai.geojson` — administrative boundary of Primorsky Krai
  (OSM relation 151225, fetched through Nominatim), simplified to 0.001°.
  Source: OpenStreetMap contributors, ODbL 1.0.
- `primorye_land.geojson` — land polygons clipped to 129.4–139.9°E,
  41.8–49.0°N and simplified to 0.0003°. Derived from GSHHS 2.3.6 full
  resolution (`basemap-data-hires` 2.0.0), LGPL-3.0-or-later. A small
  manual polygon covers the Tokarevsky lighthouse islet, which GSHHS omits.

Regeneration needs Python 3 with `numpy`, `shapely` and the
`basemap-data-hires` package. Keep the simplification values: finer data
costs binary size, coarser data drops bays and capes.
