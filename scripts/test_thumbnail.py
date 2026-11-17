import unittest

from thumbnail import crop_scale


class WindowShadowCrop(unittest.TestCase):
    def test_known_niri_shadow_extents(self):
        self.assertEqual(crop_scale(2940, 1780, 2860, 1700), (2940 / 2860, 1780 / 1700))

    def test_matching_content_has_no_crop(self):
        self.assertEqual(crop_scale(1920, 1080, 1920, 1080), (1, 1))

    def test_scale_or_geometry_mismatches_are_not_cropped(self):
        self.assertEqual(crop_scale(3840, 2160, 1920, 1080), (1, 1))
        self.assertEqual(crop_scale(1920, 1080, 3840, 2160), (1, 1))
        self.assertEqual(crop_scale(2000, 1080, 1920, 1080), (1, 1))


if __name__ == '__main__':
    unittest.main()
