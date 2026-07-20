#include <catch2/catch_test_macros.hpp>
#include <Poseidon/Graphics/Textures/TextureBank.hpp>
#include <cstring>

using namespace Poseidon;

TEST_CASE("TrySplitBlendedTerrainTextureName splits blended tiles into quadrants", "[graphics][surface]")
{
    char codes[4][3];

    SECTION("uniform blended tile")
    {
        REQUIRE(TrySplitBlendedTerrainTextureName("tatatata", codes));
        CHECK(std::strcmp(codes[0], "ta") == 0);
        CHECK(std::strcmp(codes[1], "ta") == 0);
        CHECK(std::strcmp(codes[2], "ta") == 0);
        CHECK(std::strcmp(codes[3], "ta") == 0);
    }

    SECTION("mixed quadrants preserve TL, TR, BL, BR order")
    {
        REQUIRE(TrySplitBlendedTerrainTextureName("tatatau1", codes));
        CHECK(std::strcmp(codes[0], "ta") == 0); // TL
        CHECK(std::strcmp(codes[1], "ta") == 0); // TR
        CHECK(std::strcmp(codes[2], "ta") == 0); // BL
        CHECK(std::strcmp(codes[3], "u1") == 0); // BR
    }

    SECTION("four distinct quadrants")
    {
        REQUIRE(TrySplitBlendedTerrainTextureName("j9u1tab7", codes));
        CHECK(std::strcmp(codes[0], "j9") == 0);
        CHECK(std::strcmp(codes[1], "u1") == 0);
        CHECK(std::strcmp(codes[2], "ta") == 0);
        CHECK(std::strcmp(codes[3], "b7") == 0);
    }

    SECTION("a 2-char base name is not a blended tile")
    {
        CHECK_FALSE(TrySplitBlendedTerrainTextureName("ta", codes));
    }

    SECTION("other lengths are rejected")
    {
        CHECK_FALSE(TrySplitBlendedTerrainTextureName("", codes));
        CHECK_FALSE(TrySplitBlendedTerrainTextureName("abcdef", codes));     // 6
        CHECK_FALSE(TrySplitBlendedTerrainTextureName("abcdefghij", codes)); // 10
    }
}
