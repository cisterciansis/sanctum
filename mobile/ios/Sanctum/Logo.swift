import SwiftUI

/// The Sanctum mark, traced from the logo artwork with potrace into a unit
/// square (y down). `outline` is the ring and dome (with their holes); the
/// dot is a circle so the launch animation can bloom out of it.
enum Logo {
    static let dot = CGPoint(x: 0.49049, y: 0.70301)
    static let dotRadius: CGFloat = 0.05596

    static let outline: [Path] = [
        {
            var p = Path()
            p.move(to: P(0.22131, 0.94258))
            p.addCurve(to: P(0.09430, 0.82669), control1: P(0.17606, 0.91515), control2: P(0.12684, 0.87023))
            p.addCurve(to: P(0.00322, 0.60423), control1: P(0.04465, 0.76026), control2: P(0.01481, 0.68735))
            p.addCurve(to: P(0.00369, 0.49670), control1: P(0.00000, 0.58110), control2: P(0.00025, 0.52183))
            p.addCurve(to: P(0.07171, 0.30376), control1: P(0.01345, 0.42516), control2: P(0.03560, 0.36233))
            p.addCurve(to: P(0.14317, 0.21343), control1: P(0.09414, 0.26738), control2: P(0.11304, 0.24348))
            p.addCurve(to: P(0.35950, 0.08419), control1: P(0.20561, 0.15115), control2: P(0.27509, 0.10964))
            p.addCurve(to: P(0.68505, 0.09802), control1: P(0.46818, 0.05142), control2: P(0.58089, 0.05621))
            p.addCurve(to: P(0.85953, 0.21479), control1: P(0.75288, 0.12524), control2: P(0.80979, 0.16333))
            p.addCurve(to: P(0.99015, 0.46203), control1: P(0.92757, 0.28518), control2: P(0.97041, 0.36627))
            p.addCurve(to: P(0.99874, 0.53665), control1: P(0.99325, 0.47706), control2: P(0.99794, 0.51783))
            p.addCurve(to: P(0.98931, 0.64171), control1: P(1.00000, 0.56650), control2: P(0.99642, 0.60638))
            p.addCurve(to: P(0.90712, 0.82469), control1: P(0.97639, 0.70588), control2: P(0.94760, 0.76998))
            p.addCurve(to: P(0.81256, 0.91881), control1: P(0.88238, 0.85813), control2: P(0.84665, 0.89370))
            p.addCurve(to: P(0.76634, 0.94858), control1: P(0.79358, 0.93279), control2: P(0.76907, 0.94858))
            p.addCurve(to: P(0.76555, 0.79844), control1: P(0.76591, 0.94858), control2: P(0.76555, 0.88102))
            p.addCurve(to: P(0.76317, 0.62956), control1: P(0.76555, 0.65465), control2: P(0.76545, 0.64751))
            p.addCurve(to: P(0.75198, 0.57407), control1: P(0.76030, 0.60695), control2: P(0.75750, 0.59305))
            p.addCurve(to: P(0.69060, 0.45233), control1: P(0.73913, 0.52983), control2: P(0.71845, 0.48882))
            p.addCurve(to: P(0.61101, 0.36981), control1: P(0.67076, 0.42635), control2: P(0.65030, 0.40513))
            p.addCurve(to: P(0.55204, 0.31372), control1: P(0.57469, 0.33716), control2: P(0.56455, 0.32751))
            p.addCurve(to: P(0.50671, 0.25341), control1: P(0.52948, 0.28886), control2: P(0.51886, 0.27472))
            p.addCurve(to: P(0.50106, 0.24462), control1: P(0.50396, 0.24858), control2: P(0.50142, 0.24463))
            p.addCurve(to: P(0.49781, 0.24997), control1: P(0.50071, 0.24461), control2: P(0.49924, 0.24702))
            p.addCurve(to: P(0.46957, 0.29220), control1: P(0.49308, 0.25970), control2: P(0.48093, 0.27788))
            p.addCurve(to: P(0.38075, 0.37870), control1: P(0.45014, 0.31670), control2: P(0.43106, 0.33528))
            p.addCurve(to: P(0.30885, 0.45210), control1: P(0.35445, 0.40139), control2: P(0.32298, 0.43353))
            p.addCurve(to: P(0.23593, 0.62070), control1: P(0.26893, 0.50461), control2: P(0.24521, 0.55946))
            p.addCurve(to: P(0.23348, 0.79139), control1: P(0.23408, 0.63291), control2: P(0.23384, 0.64923))
            p.addCurve(to: P(0.23214, 0.94858), control1: P(0.23326, 0.87784), control2: P(0.23266, 0.94858))
            p.addCurve(to: P(0.22131, 0.94258), control1: P(0.23163, 0.94858), control2: P(0.22675, 0.94588))
            p.closeSubpath()
            return p
        }(),
        {
            var p = Path()
            p.move(to: P(0.11551, 0.63420))
            p.addCurve(to: P(0.19310, 0.45733), control1: P(0.12489, 0.57163), control2: P(0.15122, 0.51160))
            p.addCurve(to: P(0.26960, 0.37724), control1: P(0.20992, 0.43553), control2: P(0.24305, 0.40085))
            p.addCurve(to: P(0.30662, 0.34630), control1: P(0.27509, 0.37235), control2: P(0.29175, 0.35843))
            p.addCurve(to: P(0.34824, 0.31234), control1: P(0.32149, 0.33417), control2: P(0.34021, 0.31889))
            p.addCurve(to: P(0.44783, 0.21729), control1: P(0.38718, 0.28055), control2: P(0.42667, 0.24287))
            p.addCurve(to: P(0.47816, 0.17468), control1: P(0.45819, 0.20477), control2: P(0.46981, 0.18844))
            p.addCurve(to: P(0.49884, 0.13090), control1: P(0.48418, 0.16477), control2: P(0.49658, 0.13852))
            p.addCurve(to: P(0.50115, 0.12591), control1: P(0.49965, 0.12815), control2: P(0.50069, 0.12591))
            p.addCurve(to: P(0.50463, 0.13325), control1: P(0.50161, 0.12591), control2: P(0.50317, 0.12921))
            p.addCurve(to: P(0.53290, 0.18743), control1: P(0.50891, 0.14510), control2: P(0.52382, 0.17368))
            p.addCurve(to: P(0.64509, 0.30442), control1: P(0.55647, 0.22311), control2: P(0.59493, 0.26322))
            p.addCurve(to: P(0.68152, 0.33421), control1: P(0.65155, 0.30972), control2: P(0.66795, 0.32313))
            p.addCurve(to: P(0.78347, 0.42791), control1: P(0.73467, 0.37760), control2: P(0.75380, 0.39519))
            p.addCurve(to: P(0.88380, 0.62891), control1: P(0.83995, 0.49021), control2: P(0.87086, 0.55214))
            p.addCurve(to: P(0.88653, 0.72994), control1: P(0.88561, 0.63968), control2: P(0.88596, 0.65257))
            p.addLine(to: P(0.88719, 0.81864)); p.addLine(to: P(0.89559, 0.80692))
            p.addCurve(to: P(0.96891, 0.64301), control1: P(0.93085, 0.75775), control2: P(0.95581, 0.70196))
            p.addCurve(to: P(0.97827, 0.55119), control1: P(0.97545, 0.61359), control2: P(0.97827, 0.58594))
            p.addCurve(to: P(0.96656, 0.44792), control1: P(0.97827, 0.51437), control2: P(0.97504, 0.48586))
            p.addCurve(to: P(0.89753, 0.29233), control1: P(0.95484, 0.39550), control2: P(0.93030, 0.34019))
            p.addCurve(to: P(0.80375, 0.19008), control1: P(0.87155, 0.25439), control2: P(0.83969, 0.21966))
            p.addCurve(to: P(0.45999, 0.08368), control1: P(0.70812, 0.11139), control2: P(0.58108, 0.07206))
            p.addCurve(to: P(0.16457, 0.22002), control1: P(0.34856, 0.09437), control2: P(0.24538, 0.14199))
            p.addCurve(to: P(0.09744, 0.29984), control1: P(0.13665, 0.24697), control2: P(0.11649, 0.27095))
            p.addCurve(to: P(0.04510, 0.40503), control1: P(0.07399, 0.33542), control2: P(0.05820, 0.36714))
            p.addCurve(to: P(0.03452, 0.66064), control1: P(0.01624, 0.48848), control2: P(0.01248, 0.57925))
            p.addCurve(to: P(0.07186, 0.75525), control1: P(0.04354, 0.69396), control2: P(0.05801, 0.73062))
            p.addCurve(to: P(0.10674, 0.81044), control1: P(0.08628, 0.78088), control2: P(0.09326, 0.79193))
            p.addLine(to: P(0.11270, 0.81862)); p.addLine(to: P(0.11340, 0.73111))
            p.addCurve(to: P(0.11551, 0.63420), control1: P(0.11387, 0.67260), control2: P(0.11457, 0.64049))
            p.closeSubpath()
            return p
        }(),
    ]
}

private func P(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x, y: y) }
