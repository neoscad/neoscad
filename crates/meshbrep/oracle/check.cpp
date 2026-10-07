// An OCCT read-back oracle for meshbrep's STEP files (a test tool only;
// meshbrep never links OCCT).
//
//   check FILE.step [FILE.step ...]
//
// For each file prints one JSON line: whether OCCT's BRepCheck_Analyzer
// finds the shape valid, its solids, shells and closed shells, edges
// bounding only one face ("free_edges", seams excepted), volume, area, face
// and edge types, and the largest edge or vertex tolerance after reading
// (OCCT raises tolerances where the file's geometry does not meet).
#include <BRepAdaptor_Curve.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepCheck_ListOfStatus.hxx>
#include <BRepCheck_Result.hxx>
#include <BRepCheck_Status.hxx>
#include <BRepGProp.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRep_Tool.hxx>
#include <GProp_GProps.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <STEPControl_Reader.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Shape.hxx>
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <string>

static const char *stype(GeomAbs_SurfaceType t) {
  switch (t) {
  case GeomAbs_Plane: return "plane";
  case GeomAbs_Cylinder: return "cylinder";
  case GeomAbs_Cone: return "cone";
  case GeomAbs_Sphere: return "sphere";
  case GeomAbs_Torus: return "torus";
  case GeomAbs_BSplineSurface: return "bspline";
  default: return "other";
  }
}

static const char *ctype(GeomAbs_CurveType t) {
  switch (t) {
  case GeomAbs_Line: return "line";
  case GeomAbs_Circle: return "circle";
  case GeomAbs_Ellipse: return "ellipse";
  case GeomAbs_BSplineCurve: return "bspline";
  default: return "other";
  }
}

// With $VERBOSE set, the first failures of BRepCheck_Analyzer on stderr:
// sub-shape kind, index, and status codes (BRepCheck_Status).
static void explain(const TopoDS_Shape &s, BRepCheck_Analyzer &an) {
  const TopAbs_ShapeEnum kinds[] = {TopAbs_VERTEX, TopAbs_EDGE, TopAbs_WIRE, TopAbs_FACE, TopAbs_SHELL, TopAbs_SOLID};
  const char *names[] = {"vertex", "edge", "wire", "face", "shell", "solid"};
  int shown = 0;
  for (int k = 0; k < 6 && shown < 20; k++) {
    TopTools_IndexedMapOfShape m;
    TopExp::MapShapes(s, kinds[k], m);
    for (int i = 1; i <= m.Extent() && shown < 20; i++) {
      Handle(BRepCheck_Result) r = an.Result(m(i));
      if (r.IsNull()) continue;
      for (BRepCheck_ListIteratorOfListOfStatus it(r->Status()); it.More(); it.Next()) {
        if (it.Value() != BRepCheck_NoError) {
          fprintf(stderr, "  %s %d: status %d\n", names[k], i, (int)it.Value());
          shown++;
        }
      }
    }
  }
}

static void report(const char *file, const TopoDS_Shape &s) {
  BRepCheck_Analyzer an(s);
  bool valid = !s.IsNull() && an.IsValid();
  if (!valid && getenv("VERBOSE")) explain(s, an);
  // Adaptive integration to a relative 1e-9: the fixed-order default
  // misjudges faces bounded by long B-spline parameter curves.
  GProp_GProps vp, sp, vfix;
  const double vol_err = BRepGProp::VolumeProperties(s, vp, 1e-9);
  BRepGProp::SurfaceProperties(s, sp, 1e-9);
  BRepGProp::VolumeProperties(s, vfix);
  // And by OCCT's own fine triangulation of the faces, independent of
  // both integrators, when $MESH_DEFLECTION is set (fine deflections need
  // gigabytes near sphere poles; 0 otherwise).
  GProp_GProps vmesh;
  const char *defl = getenv("MESH_DEFLECTION");
  if (defl) {
    BRepMesh_IncrementalMesh(s, atof(defl), false, 0.01);
    BRepGProp::VolumeProperties(s, vmesh, false, false, true);
  }
  int solids = 0, shells = 0, closed_shells = 0;
  for (TopExp_Explorer e(s, TopAbs_SOLID); e.More(); e.Next()) solids++;
  for (TopExp_Explorer e(s, TopAbs_SHELL); e.More(); e.Next()) {
    shells++;
    if (BRep_Tool::IsClosed(e.Current())) closed_shells++;
  }
  std::map<std::string, int> ft, et;
  TopTools_IndexedMapOfShape faces, edges, verts;
  TopExp::MapShapes(s, TopAbs_FACE, faces);
  TopExp::MapShapes(s, TopAbs_EDGE, edges);
  TopExp::MapShapes(s, TopAbs_VERTEX, verts);
  for (int i = 1; i <= faces.Extent(); i++)
    ft[stype(BRepAdaptor_Surface(TopoDS::Face(faces(i))).GetType())]++;
  double maxtol = 0;
  for (int i = 1; i <= edges.Extent(); i++) {
    const TopoDS_Edge &e = TopoDS::Edge(edges(i));
    if (BRep_Tool::Degenerated(e)) {
      et["degenerate"]++;
      continue;
    }
    et[ctype(BRepAdaptor_Curve(e).GetType())]++;
    maxtol = std::max(maxtol, BRep_Tool::Tolerance(e));
  }
  for (int i = 1; i <= verts.Extent(); i++)
    maxtol = std::max(maxtol, BRep_Tool::Tolerance(TopoDS::Vertex(verts(i))));
  TopTools_IndexedDataMapOfShapeListOfShape ef;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, ef);
  int free_edges = 0;
  for (int i = 1; i <= ef.Extent(); i++) {
    const TopoDS_Edge &e = TopoDS::Edge(ef.FindKey(i));
    if (BRep_Tool::Degenerated(e)) continue;
    if (ef(i).Extent() == 1 && !BRep_Tool::IsClosed(e, TopoDS::Face(ef(i).First()))) free_edges++;
  }
  printf("{\"file\":\"%s\",\"valid\":%s,\"solids\":%d,\"shells\":%d,\"closed_shells\":%d,"
         "\"volume\":%.10f,\"volume_err\":%.2g,\"volume_fixed\":%.10f,\"volume_mesh\":%.10f,\"area\":%.10f,\"faces\":%d,\"edges\":%d,\"free_edges\":%d,"
         "\"max_tol\":%.3g,\"face_types\":{",
         file, valid ? "true" : "false", solids, shells, closed_shells, vp.Mass(), vol_err, vfix.Mass(), vmesh.Mass(), sp.Mass(),
         faces.Extent(), edges.Extent(), free_edges, maxtol);
  bool first = true;
  for (auto &kv : ft) {
    printf("%s\"%s\":%d", first ? "" : ",", kv.first.c_str(), kv.second);
    first = false;
  }
  printf("},\"edge_types\":{");
  first = true;
  for (auto &kv : et) {
    printf("%s\"%s\":%d", first ? "" : ",", kv.first.c_str(), kv.second);
    first = false;
  }
  printf("}}\n");
  fflush(stdout);
}

int main(int argc, char **argv) {
  for (int i = 1; i < argc; i++) {
    STEPControl_Reader r;
    if (r.ReadFile(argv[i]) != IFSelect_RetDone) {
      printf("{\"file\":\"%s\",\"error\":\"read\"}\n", argv[i]);
      continue;
    }
    r.TransferRoots();
    report(argv[i], r.OneShape());
  }
  return 0;
}
