// OCCT's own fillets and chamfers of small CSG solids, for comparing the
// volumes of meshbrep's blends between curved faces with another kernel's
// (a test tool only; meshbrep never links OCCT).
//
//   fillet fillet R CSG...
//   fillet chamfer D CSG...
//
// CSG is a solid in prefix notation:
//
//   union A B | cut A B
//   cyl R H X Y Z AX AY AZ     radius R, height H, base centre, axis
//   box X Y Z DX DY DZ         corner and sizes
//   sphere R X Y Z
//
// Every edge of the solid whose curve is neither a line nor a circle (the
// junctions of the operands: B-spline or ellipse) is blended with radius R
// (or chamfered by distance D), and one JSON line is printed: whether the
// result is done and valid, how many edges were blended, and its volume
// by OCCT's two integrators (adaptive to 1e-9, and fixed order).

#include <BRepAdaptor_Curve.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepGProp.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <BRepPrimAPI_MakeSphere.hxx>
#include <GProp_GProps.hxx>
#include <Standard_Failure.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopoDS.hxx>
#include <cstdio>
#include <cstdlib>
#include <string>

static int pos = 0;
static int argc_;
static char **argv_;

static double num() {
  if (pos >= argc_) {
    fprintf(stderr, "missing number\n");
    exit(2);
  }
  return atof(argv_[pos++]);
}

static TopoDS_Shape solid() {
  if (pos >= argc_) {
    fprintf(stderr, "missing solid\n");
    exit(2);
  }
  std::string w = argv_[pos++];
  if (w == "union" || w == "cut") {
    TopoDS_Shape a = solid();
    TopoDS_Shape b = solid();
    if (w == "union") return BRepAlgoAPI_Fuse(a, b).Shape();
    return BRepAlgoAPI_Cut(a, b).Shape();
  }
  if (w == "cyl") {
    double r = num(), h = num(), x = num(), y = num(), z = num(), ax = num(), ay = num(), az = num();
    return BRepPrimAPI_MakeCylinder(gp_Ax2(gp_Pnt(x, y, z), gp_Dir(ax, ay, az)), r, h).Shape();
  }
  if (w == "box") {
    double x = num(), y = num(), z = num(), dx = num(), dy = num(), dz = num();
    return BRepPrimAPI_MakeBox(gp_Pnt(x, y, z), dx, dy, dz).Shape();
  }
  if (w == "sphere") {
    double r = num(), x = num(), y = num(), z = num();
    return BRepPrimAPI_MakeSphere(gp_Pnt(x, y, z), r).Shape();
  }
  fprintf(stderr, "unknown solid %s\n", w.c_str());
  exit(2);
}

int main(int argc, char **argv) {
  argc_ = argc;
  argv_ = argv;
  pos = 1;
  if (argc < 3) {
    fprintf(stderr, "usage: fillet fillet|chamfer SIZE CSG...\n");
    return 2;
  }
  std::string op = argv[pos++];
  double size = num();
  TopoDS_Shape s = solid();
  TopTools_IndexedMapOfShape edges;
  TopExp::MapShapes(s, TopAbs_EDGE, edges);
  int n = 0;
  bool done = false, valid = false;
  double vol = 0, vfix = 0, err = 0;
  try {
    TopoDS_Shape out;
    if (op == "fillet") {
      BRepFilletAPI_MakeFillet mk(s);
      for (int i = 1; i <= edges.Extent(); i++) {
        const TopoDS_Edge &e = TopoDS::Edge(edges(i));
        GeomAbs_CurveType t = BRepAdaptor_Curve(e).GetType();
        if (t != GeomAbs_Line && t != GeomAbs_Circle) {
          mk.Add(size, e);
          n++;
        }
      }
      mk.Build();
      done = mk.IsDone();
      if (done) out = mk.Shape();
    } else {
      BRepFilletAPI_MakeChamfer mk(s);
      for (int i = 1; i <= edges.Extent(); i++) {
        const TopoDS_Edge &e = TopoDS::Edge(edges(i));
        GeomAbs_CurveType t = BRepAdaptor_Curve(e).GetType();
        if (t != GeomAbs_Line && t != GeomAbs_Circle) {
          mk.Add(size, e);
          n++;
        }
      }
      mk.Build();
      done = mk.IsDone();
      if (done) out = mk.Shape();
    }
    if (done) {
      valid = BRepCheck_Analyzer(out).IsValid();
      GProp_GProps p, q;
      err = BRepGProp::VolumeProperties(out, p, 1e-9);
      BRepGProp::VolumeProperties(out, q);
      vol = p.Mass();
      vfix = q.Mass();
    }
  } catch (Standard_Failure &f) {
    printf("{\"done\":false,\"error\":\"%s\",\"edges\":%d}\n", f.GetMessageString(), n);
    return 0;
  }
  printf("{\"done\":%s,\"valid\":%s,\"edges\":%d,\"volume\":%.10f,\"volume_err\":%.2g,\"volume_fixed\":%.10f}\n",
         done ? "true" : "false", valid ? "true" : "false", n, vol, err, vfix);
  return 0;
}
