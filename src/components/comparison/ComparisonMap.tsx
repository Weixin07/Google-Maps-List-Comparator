import { useEffect, useMemo, useRef } from "react";
import maplibregl, { Map as MapLibreMap, GeoJSONSource } from "maplibre-gl";
import type {
  ComparisonSegmentKey,
  PlaceComparisonRow,
} from "../../types/comparison";

const defaultStyle = "https://demotiles.maplibre.org/style.json";

const layerColors: Record<ComparisonSegmentKey, string> = {
  overlap: "#16a34a",
  only_a: "#0ea5e9",
  only_b: "#9333ea",
};

type FocusPoint = { lng: number; lat: number } | null;

type ComparisonMapProps = {
  styleUrl?: string | null;
  data: Record<ComparisonSegmentKey, PlaceComparisonRow[]>;
  selectedIds: Set<string>;
  focusedPlaceId: string | null;
  focusPoint: FocusPoint;
  visibility: Record<ComparisonSegmentKey, boolean>;
  onMarkerFocus?: (placeId: string) => void;
};

export function ComparisonMap({
  styleUrl,
  data,
  selectedIds,
  focusedPlaceId,
  focusPoint,
  visibility,
  onMarkerFocus,
}: ComparisonMapProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<MapLibreMap | null>(null);

  const geojson = useMemo(() => {
    const features = (Object.keys(data) as ComparisonSegmentKey[]).flatMap(
      (segment) =>
        data[segment].map((row) => ({
          type: "Feature" as const,
          geometry: {
            type: "Point" as const,
            coordinates: [row.lng, row.lat],
          },
          properties: {
            place_id: row.place_id,
            name: row.name,
            segment,
            selected: selectedIds.has(row.place_id),
          },
        })),
    );
    return {
      type: "FeatureCollection" as const,
      features,
    };
  }, [data, selectedIds]);
  const geojsonRef = useRef(geojson);

  useEffect(() => {
    if (!containerRef.current || mapRef.current) {
      return;
    }
    const map = new maplibregl.Map({
      container: containerRef.current,
      style: styleUrl ?? defaultStyle,
      center: [-98.5795, 39.8283],
      zoom: 3.5,
      attributionControl: false,
    });
    mapRef.current = map;
    map.addControl(new maplibregl.NavigationControl(), "top-left");
    map.on("load", () => {
      map.addSource("comparison-places", {
        type: "geojson",
        data: {
          type: "FeatureCollection",
          features: [],
        },
      });
      const baseSource = map.getSource("comparison-places") as GeoJSONSource | undefined;
      baseSource?.setData(geojsonRef.current);
      (Object.keys(layerColors) as ComparisonSegmentKey[]).forEach(
        (segment) => {
          map.addLayer({
            id: `comparison-${segment}`,
            type: "circle",
            source: "comparison-places",
            filter: ["==", ["get", "segment"], segment],
            paint: {
              "circle-radius": 6,
              "circle-color": layerColors[segment],
              "circle-opacity": 0.8,
              "circle-stroke-color": "#0f172a",
              "circle-stroke-width": [
                "case",
                ["==", ["get", "selected"], true],
                2,
                0.5,
              ],
            },
          });
          map.on("click", `comparison-${segment}`, (event) => {
            const placeId = event.features?.[0]?.properties?.place_id as
              | string
              | undefined;
            if (placeId && onMarkerFocus) {
              onMarkerFocus(placeId);
            }
          });
          map.on("mouseenter", `comparison-${segment}`, () => {
            map.getCanvas().style.cursor = "pointer";
          });
          map.on("mouseleave", `comparison-${segment}`, () => {
            map.getCanvas().style.cursor = "";
          });
        },
      );

      map.addLayer({
        id: "comparison-focus",
        type: "circle",
        source: "comparison-places",
        filter: ["==", ["get", "place_id"], ""],
        paint: {
          "circle-radius": 10,
          "circle-color": "rgba(253,224,71,0.4)",
          "circle-stroke-color": "#fde047",
          "circle-stroke-width": 2,
        },
      });
    });

    return () => {
      map.remove();
      mapRef.current = null;
    };
  }, [onMarkerFocus, styleUrl]);

  useEffect(() => {
    geojsonRef.current = geojson;
    const map = mapRef.current;
    if (!map) {
      return;
    }
    const source = map.getSource("comparison-places") as GeoJSONSource | undefined;
    source?.setData(geojson);
  }, [geojson]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map) {
      return;
    }
    (Object.keys(layerColors) as ComparisonSegmentKey[]).forEach((segment) => {
      const layerId = `comparison-${segment}`;
      if (map.getLayer(layerId)) {
        map.setLayoutProperty(
          layerId,
          "visibility",
          visibility[segment] ? "visible" : "none",
        );
      }
    });
  }, [visibility]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !focusPoint) {
      return;
    }
    map.flyTo({
      center: [focusPoint.lng, focusPoint.lat],
      zoom: Math.max(map.getZoom(), 12),
    });
  }, [focusPoint]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !map.getLayer("comparison-focus")) {
      return;
    }
    map.setFilter("comparison-focus", [
      "==",
      ["get", "place_id"],
      focusedPlaceId ?? "",
    ]);
  }, [focusedPlaceId]);

  return (
    <div className="comparison-map">
      <div ref={containerRef} className="comparison-map__canvas" />
    </div>
  );
}
